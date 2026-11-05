//! F-Droid repositories, index format v2.
//!
//! A repository signs `entry.jar` with its key: a JAR signature over `entry.json`, which names
//! `index-v2.json`, and diffs from older indexes to it, each by size and SHA-256. So only the signed
//! entry is trusted for what it says, and everything else is trusted for matching it:
//!
//! 1. `entry.jar` is verified as Android verifies a JAR signature, and its one signer's certificate
//!    must have the fingerprint pinned for the source.
//! 2. An entry older than the cached index is refused, so an old signed entry cannot roll it back.
//! 3. A diff from the cached index, else the whole index, is read up to its signed size and must have
//!    its signed SHA-256.
//!
//! The cache keeps only the fields uxdroid reads, in index-v2's own shape (`index.json`), so a diff,
//! a JSON merge patch (RFC 7386) of the whole index, applies to it as it does to the whole index.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::SourceConfig;
use crate::fetch::Fetch;
use crate::{App, Error, Refresh, Release, valid_package, write_atomically};

/// Entries seen are a few KiB; the limit only stops a hostile server.
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024;
/// F-Droid's whole index is 72 MB in November 2026.
const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_NAME: usize = 512;
/// Larger APKs are not published by F-Droid; the limit only bounds a hostile index.
pub(crate) const MAX_APK_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Entry {
    timestamp: u64,
    index: FileRef,
    #[serde(default)]
    diffs: BTreeMap<String, FileRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct FileRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

// The cached index. Every field is optional, so dropping fields is the same before and after a merge
// patch, and a patch that leaves an entry incomplete only hides that entry.

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    packages: BTreeMap<String, Package>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Package {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
    #[serde(default)]
    versions: BTreeMap<String, Version>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_updated: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Version {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<FileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest: Option<VersionManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_channels: Option<Vec<String>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_code: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uses_sdk: Option<UsesSdk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_sdk_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nativecode: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signer: Option<Signer>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsesSdk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_sdk_version: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Signer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<Vec<String>>,
}

/// What is known of the cached index beside it.
#[derive(Debug, Serialize, Deserialize)]
struct State {
    url: String,
    fingerprint: String,
    /// The signed entry's timestamp, in milliseconds: the index it names, and the key of diffs from it.
    timestamp: u64,
    /// When the repository was last asked for a newer entry, in seconds since the epoch.
    checked: u64,
}

/// An F-Droid repository and its cache.
pub struct Fdroid {
    config: SourceConfig,
    cache: PathBuf,
}

/// The apps of a source and what to tell the user about how fresh they are.
pub struct Loaded {
    pub apps: Vec<App>,
    pub note: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// A repository file name as an index gives it: `/` followed by segments of letters, digits and
/// `._+~-`, none of them `.` or `..`. Anything else could leave the repository.
pub(crate) fn valid_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('/') else {
        return false;
    };
    name.len() <= MAX_FILE_NAME
        && rest.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'~' | b'-')
                })
        })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// RFC 7386: objects merge key by key, `null` removes a key, anything else replaces.
fn merge(target: &mut Value, patch: Value) {
    let Value::Object(patch) = patch else {
        *target = patch;
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let Value::Object(map) = target else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            map.remove(&key);
        } else {
            merge(map.entry(key).or_insert(Value::Null), value);
        }
    }
}

/// The text for the user's language among an index's localized texts: US English, English, then the
/// first one.
fn localized(texts: Option<&BTreeMap<String, String>>) -> Option<String> {
    let texts = texts?;
    ["en-US", "en"]
        .iter()
        .find_map(|locale| texts.get(*locale))
        .or_else(|| texts.values().next())
        .cloned()
}

impl Fdroid {
    pub fn new(config: SourceConfig, store: &Path) -> Self {
        let cache = store.join("sources").join(&config.name);
        Self { config, cache }
    }

    fn url(&self, name: &str) -> String {
        format!("{}{name}", self.config.url)
    }

    fn untrusted(&self, what: impl std::fmt::Display) -> Error {
        Error::new(format!(
            "the index of {} ({}) is refused: {what}",
            self.config.name, self.config.url
        ))
    }

    /// Reads `file` up to its signed size and checks it has its signed SHA-256.
    fn fetch_signed(
        &self,
        fetch: &dyn Fetch,
        file: &FileRef,
        what: &str,
    ) -> Result<Vec<u8>, Error> {
        let (Some(name), Some(sha256), Some(size)) = (&file.name, &file.sha256, file.size) else {
            return Err(self.untrusted(format!(
                "its signed entry does not give the {what}'s name, size and SHA-256"
            )));
        };
        if !valid_file_name(name) {
            return Err(self.untrusted(format!(
                "its signed entry names the {what} {}, which is not a file of the repository",
                name.escape_debug()
            )));
        }
        if size > MAX_INDEX_BYTES {
            return Err(self.untrusted(format!(
                "its {what} would be {size} bytes, more than the {MAX_INDEX_BYTES} allowed"
            )));
        }
        let bytes = fetch.get(&self.url(name), size).map_err(|error| {
            if error.0.contains("is larger than") {
                self.untrusted(format!("its {what} is larger than its signed entry says"))
            } else {
                error
            }
        })?;
        if bytes.len() as u64 != size {
            return Err(self.untrusted(format!(
                "its {what} has {} bytes, and its signed entry says {size}",
                bytes.len()
            )));
        }
        if hex(&Sha256::digest(&bytes)) != sha256.to_ascii_lowercase() {
            return Err(self.untrusted(format!(
                "its {what} does not have the SHA-256 its signed entry gives, so it was changed after signing"
            )));
        }
        Ok(bytes)
    }

    /// The verified `entry.json` of the repository.
    fn entry(&self, fetch: &dyn Fetch) -> Result<Entry, Error> {
        let jar = fetch.get(&self.url("/entry.jar"), MAX_ENTRY_BYTES)?;
        let signature = uxdroid_apk::verify_jar_signature(&jar)
            .map_err(|error| self.untrusted(format!("entry.jar does not verify: {error}")))?;
        let fingerprints = signature.fingerprints();
        if fingerprints != [self.config.fingerprint.as_str()] {
            return Err(self.untrusted(format!(
                "entry.jar is signed by {}, not by the certificate pinned for the source, {}",
                fingerprints.join(", "),
                self.config.fingerprint
            )));
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&jar[..]))
            .map_err(|error| self.untrusted(format!("entry.jar is not a JAR: {error}")))?;
        let entry = archive
            .by_name("entry.json")
            .map_err(|_| self.untrusted("entry.jar has no entry.json"))?;
        let mut bytes = Vec::new();
        entry
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| self.untrusted(format!("cannot read entry.json: {error}")))?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            return Err(self.untrusted("entry.json is too large"));
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| self.untrusted(format!("entry.json is malformed: {error}")))
    }

    fn read_cache(&self) -> Option<(State, Index)> {
        let state: State =
            serde_json::from_slice(&std::fs::read(self.cache.join("state.json")).ok()?).ok()?;
        // A cache made for another address or key is not this source's.
        if state.url != self.config.url || state.fingerprint != self.config.fingerprint {
            return None;
        }
        let index: Index =
            serde_json::from_slice(&std::fs::read(self.cache.join("index.json")).ok()?).ok()?;
        Some((state, index))
    }

    fn write_state(&self, timestamp: u64) -> Result<(), Error> {
        let state = State {
            url: self.config.url.clone(),
            fingerprint: self.config.fingerprint.clone(),
            timestamp,
            checked: now(),
        };
        let bytes = serde_json::to_vec(&state).map_err(|error| Error::new(error.to_string()))?;
        write_atomically(&self.cache.join("state.json"), &bytes)
    }

    /// Fetches what is newer than `cached`, verified, and replaces the cache with it.
    fn refresh(
        &self,
        fetch: &dyn Fetch,
        cached: Option<(State, Index)>,
        progress: &dyn Fn(&str),
    ) -> Result<Index, Error> {
        let entry = self.entry(fetch)?;
        if let Some((state, index)) = cached {
            if entry.timestamp < state.timestamp {
                return Err(self.untrusted(format!(
                    "its signed entry is from {}, older than the index already read ({}), so it may be a replay",
                    entry.timestamp, state.timestamp
                )));
            }
            if entry.timestamp == state.timestamp {
                self.write_state(entry.timestamp)?;
                return Ok(index);
            }
            if let Some(diff) = entry.diffs.get(&state.timestamp.to_string()) {
                progress(&format!(
                    "fetching the changes to the index of {} ({} bytes)",
                    self.config.name,
                    diff.size.unwrap_or(0)
                ));
                let patch = self.fetch_signed(fetch, diff, "diff")?;
                let patch: Value = serde_json::from_slice(&patch)
                    .map_err(|error| self.untrusted(format!("its diff is malformed: {error}")))?;
                let mut value =
                    serde_json::to_value(&index).map_err(|error| Error::new(error.to_string()))?;
                drop(index);
                merge(&mut value, patch);
                let index: Index = serde_json::from_value(value).map_err(|error| {
                    self.untrusted(format!("its diff makes a malformed index: {error}"))
                })?;
                self.write(&index, entry.timestamp)?;
                return Ok(index);
            }
        }
        progress(&format!(
            "fetching the index of {} ({} bytes)",
            self.config.name,
            entry.index.size.unwrap_or(0)
        ));
        let bytes = self.fetch_signed(fetch, &entry.index, "index")?;
        let index: Index = serde_json::from_slice(&bytes)
            .map_err(|error| self.untrusted(format!("its index is malformed: {error}")))?;
        drop(bytes);
        self.write(&index, entry.timestamp)?;
        Ok(index)
    }

    fn write(&self, index: &Index, timestamp: u64) -> Result<(), Error> {
        let bytes = serde_json::to_vec(index).map_err(|error| Error::new(error.to_string()))?;
        write_atomically(&self.cache.join("index.json"), &bytes)?;
        self.write_state(timestamp)
    }

    /// The repository's apps, from the cache, refreshed first as `refresh` says. A failed refresh
    /// falls back to the cache, with a note, and fails only when there is none.
    pub fn load(
        &self,
        fetch: &dyn Fetch,
        refresh: Refresh,
        max_age_hours: u64,
        progress: &dyn Fn(&str),
    ) -> Result<Loaded, Error> {
        std::fs::create_dir_all(&self.cache).map_err(|error| {
            Error::new(format!("cannot write {}: {error}", self.cache.display()))
        })?;
        let _lock = crate::lock(&self.cache.join(".lock"))?;
        let cached = self.read_cache();
        let age = cached
            .as_ref()
            .map(|(state, _)| now().saturating_sub(state.checked));
        let stale = age.is_none_or(|age| age >= max_age_hours.saturating_mul(3600));
        let wanted = match refresh {
            Refresh::Always => true,
            Refresh::IfStale => stale,
        };
        if !wanted {
            let (_, index) = cached.expect("a cache that is not stale exists");
            return Ok(Loaded {
                apps: self.apps(index),
                note: None,
            });
        }
        let had_cache = cached.is_some();
        let checked = cached.as_ref().map(|(state, _)| state.checked);
        match self.refresh(fetch, cached, progress) {
            Ok(index) => Ok(Loaded {
                apps: self.apps(index),
                note: None,
            }),
            Err(error) if had_cache => {
                // The cache was moved into the failed refresh; read it again.
                let index = self
                    .read_cache()
                    .map(|(_, index)| index)
                    .unwrap_or_default();
                let hours = now().saturating_sub(checked.unwrap_or(0)) / 3600;
                Ok(Loaded {
                    apps: self.apps(index),
                    note: Some(format!(
                        "the index of {} was last checked {hours} hours ago and could not be refreshed: {error}",
                        self.config.name
                    )),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// The apps an index lists, without entries that cannot be installed safely: a package name that
    /// is not one, or a version without a repository file, size and SHA-256.
    fn apps(&self, index: Index) -> Vec<App> {
        let mut apps = Vec::new();
        for (package, entry) in index.packages {
            if !valid_package(&package) {
                continue;
            }
            let metadata = entry.metadata.unwrap_or_default();
            let mut releases: Vec<Release> = entry
                .versions
                .into_values()
                .filter_map(|version| {
                    let file = version.file?;
                    let manifest = version.manifest?;
                    let name = file.name.filter(|name| valid_file_name(name))?;
                    let sha256 = file
                        .sha256
                        .map(|sha| sha.to_ascii_lowercase())
                        .filter(|sha| {
                            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
                        })?;
                    Some(Release {
                        version_name: manifest.version_name.unwrap_or_default(),
                        version_code: manifest.version_code?,
                        url: self.url(&name),
                        sha256,
                        size: file.size.filter(|size| *size <= MAX_APK_BYTES)?,
                        min_sdk: manifest
                            .uses_sdk
                            .and_then(|sdk| sdk.min_sdk_version)
                            .unwrap_or(1),
                        max_sdk: manifest.max_sdk_version,
                        abis: manifest.nativecode.unwrap_or_default(),
                        signers: manifest
                            .signer
                            .and_then(|signer| signer.sha256)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|sha| sha.to_ascii_lowercase())
                            .collect(),
                        added: version.added,
                        // Only the stable channel, as F-Droid's client offers by default.
                        stable: version
                            .release_channels
                            .is_none_or(|channels| channels.is_empty()),
                    })
                })
                .collect();
            if releases.is_empty() {
                continue;
            }
            releases.sort_by_key(|release| std::cmp::Reverse(release.version_code));
            apps.push(App {
                source: self.config.name.clone(),
                name: localized(metadata.name.as_ref()).unwrap_or_else(|| package.clone()),
                summary: localized(metadata.summary.as_ref()).unwrap_or_default(),
                package,
                updated: metadata.last_updated,
                releases,
            });
        }
        apps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_patch_merges_objects_removes_nulls_and_replaces_the_rest() {
        let mut target = serde_json::json!({"a": {"b": 1, "c": 2}, "d": [1], "e": "x"});
        merge(
            &mut target,
            serde_json::json!({"a": {"b": null, "f": 3}, "d": [2, 3], "g": {"h": null}}),
        );
        assert_eq!(
            target,
            serde_json::json!({"a": {"c": 2, "f": 3}, "d": [2, 3], "e": "x", "g": {}})
        );
    }

    #[test]
    fn repository_file_names_cannot_leave_the_repository() {
        for good in [
            "/net.gsantner.markor_164.apk",
            "/diff/1791949126460.json",
            "/a+b~c-d",
        ] {
            assert!(valid_file_name(good), "{good}");
        }
        for bad in [
            "net.apk",
            "/../x.apk",
            "/a/../b",
            "//x",
            "/a//b",
            "/./a",
            "/a b",
            "/a?b",
            "/a%2e",
            "/a\\b",
            "/",
            "/a/",
        ] {
            assert!(!valid_file_name(bad), "{bad}");
        }
    }
}
