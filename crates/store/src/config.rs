//! The sources a data directory installs from, kept in `<data>/store/sources.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// The name of F-Droid's official repository, which every data directory starts with.
pub const FDROID: &str = "fdroid";
pub const FDROID_URL: &str = "https://f-droid.org/repo";
/// The SHA-256 of the certificate that signs F-Droid's official repository index, as F-Droid
/// publishes it in "Release Channels and Signing Keys"
/// (https://f-droid.org/docs/Release_Channels_and_Signing_Keys/). Checked against the certificate of
/// https://f-droid.org/repo/entry.jar on 2026-11-04 (CN=Ciaran Gultnieks, RSA 2048).
pub const FDROID_FINGERPRINT: &str =
    "43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab";
/// An index older than this is fetched again before it is used.
pub const DEFAULT_REFRESH_AFTER_HOURS: u64 = 24;
const MAX_NAME: usize = 32;
const MAX_URL: usize = 2048;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An F-Droid repository: a signed `entry.jar` naming an `index-v2.json` by its hash.
    Fdroid,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Fdroid => "fdroid",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub name: String,
    pub kind: Kind,
    /// The repository's address, without a final `/`.
    pub url: String,
    /// The SHA-256 of the certificate its index must be signed with, in lowercase hex.
    pub fingerprint: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub refresh_after_hours: u64,
    pub sources: Vec<SourceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            refresh_after_hours: DEFAULT_REFRESH_AFTER_HOURS,
            sources: vec![SourceConfig {
                name: FDROID.to_string(),
                kind: Kind::Fdroid,
                url: FDROID_URL.to_string(),
                fingerprint: FDROID_FINGERPRINT.to_string(),
                enabled: true,
            }],
        }
    }
}

/// Names become directory names and command arguments: lowercase letters, digits and `-`.
pub fn valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// A repository address: http or https, printable, with no query, fragment or final `/`.
pub fn normalize_url(url: &str) -> Result<String, Error> {
    let url = url.trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let valid = rest.is_some_and(|rest| {
        !rest.is_empty() && !rest.starts_with('/') && !rest.contains(['?', '#', '\\'])
    }) && url.len() <= MAX_URL
        && url.bytes().all(|byte| byte.is_ascii_graphic());
    if valid {
        Ok(url.to_string())
    } else {
        Err(Error::new(format!(
            "{} is not a repository address: give an http or https URL without a query",
            url.escape_debug()
        )))
    }
}

/// A SHA-256 fingerprint as lowercase hex; colons and spaces, as some tools print it, are dropped.
pub fn normalize_fingerprint(fingerprint: &str) -> Result<String, Error> {
    let hex: String = fingerprint
        .chars()
        .filter(|c| *c != ':' && *c != ' ')
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(hex)
    } else {
        Err(Error::new(format!(
            "{} is not a SHA-256 fingerprint: give the 64 hex digits of the repository's signing certificate",
            fingerprint.escape_debug()
        )))
    }
}

impl Config {
    /// The configuration in `path`, or the default one when there is none.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let bytes = match std::fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_CONFIG_BYTES => {
                return Err(Error::new(format!("{} is too large", path.display())));
            }
            Ok(_) => std::fs::read(path)
                .map_err(|error| Error::new(format!("cannot read {}: {error}", path.display())))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(Error::new(format!(
                    "cannot read {}: {error}",
                    path.display()
                )));
            }
        };
        let config: Config = serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(format!("{} is malformed: {error}", path.display())))?;
        if config.version != 1 {
            return Err(Error::new(format!(
                "{} has version {}, which this uxdroid does not know",
                path.display(),
                config.version
            )));
        }
        for source in &config.sources {
            if !valid_source_name(&source.name)
                || normalize_url(&source.url).as_deref() != Ok(source.url.as_str())
                || normalize_fingerprint(&source.fingerprint).as_deref()
                    != Ok(source.fingerprint.as_str())
            {
                return Err(Error::new(format!(
                    "{} has a malformed source {}",
                    path.display(),
                    source.name.escape_debug()
                )));
            }
        }
        Ok(config)
    }

    /// Writes the configuration to `path`, replacing the old one at once.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|error| Error::new(error.to_string()))?;
        bytes.push(b'\n');
        crate::write_atomically(path, &bytes)
    }

    pub fn source(&self, name: &str) -> Option<&SourceConfig> {
        self.sources.iter().find(|source| source.name == name)
    }

    /// Adds an F-Droid repository. Its name must be new.
    pub fn add(
        &mut self,
        name: &str,
        url: &str,
        fingerprint: &str,
    ) -> Result<&SourceConfig, Error> {
        if !valid_source_name(name) {
            return Err(Error::new(format!(
                "{} is not a source name: use at most {MAX_NAME} lowercase letters, digits and -",
                name.escape_debug()
            )));
        }
        if self.source(name).is_some() {
            return Err(Error::new(format!(
                "there is already a source named {name}"
            )));
        }
        let url = normalize_url(url)?;
        let fingerprint = normalize_fingerprint(fingerprint)?;
        self.sources.push(SourceConfig {
            name: name.to_string(),
            kind: Kind::Fdroid,
            url,
            fingerprint,
            enabled: true,
        });
        Ok(self.sources.last().expect("just pushed"))
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), Error> {
        let source = self
            .sources
            .iter_mut()
            .find(|source| source.name == name)
            .ok_or_else(|| unknown_source(name))?;
        source.enabled = enabled;
        Ok(())
    }
}

pub(crate) fn unknown_source(name: &str) -> Error {
    Error::new(format!(
        "there is no source named {}: uxdroid sources lists them",
        name.escape_debug()
    ))
}
