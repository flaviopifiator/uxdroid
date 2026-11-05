//! Builds F-Droid repositories in index format v2, as `fdroid update` writes them: `entry.jar`
//! with a JAR signature over `entry.json`, which names `index-v2.json` and diffs by size and
//! SHA-256, and the APKs beside them.

use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Apk, Key};

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One version of an app in the index.
#[derive(Clone)]
pub struct Version {
    pub apk: Vec<u8>,
    /// The repository file, `/<package>_<code>.apk` by default.
    pub file: String,
    pub version_name: String,
    pub version_code: u64,
    pub min_sdk: u32,
    pub max_sdk: Option<u32>,
    pub nativecode: Vec<String>,
    /// The signer fingerprints the index gives; `None` leaves the field out.
    pub signer: Option<Vec<String>>,
    pub release_channels: Vec<String>,
    /// What the index says of the file, when it should lie.
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

impl Version {
    /// A version of `package` with the APK `apk`, signed by `signer`.
    pub fn new(package: &str, code: u64, name: &str, apk: Vec<u8>, signer: &Key) -> Self {
        Self {
            apk,
            file: format!("/{package}_{code}.apk"),
            version_name: name.to_string(),
            version_code: code,
            min_sdk: 21,
            max_sdk: None,
            nativecode: Vec::new(),
            signer: Some(vec![signer.fingerprint()]),
            release_channels: Vec::new(),
            sha256: None,
            size: None,
        }
    }
}

/// An app in the index.
#[derive(Clone)]
pub struct App {
    pub package: String,
    pub name: String,
    pub summary: String,
    pub last_updated: u64,
    pub versions: Vec<Version>,
}

impl App {
    pub fn new(package: &str, name: &str, summary: &str, last_updated: u64) -> Self {
        Self {
            package: package.to_string(),
            name: name.to_string(),
            summary: summary.to_string(),
            last_updated,
            versions: Vec::new(),
        }
    }

    pub fn version(mut self, version: Version) -> Self {
        self.versions.push(version);
        self
    }
}

/// `index-v2.json` for `apps`, with the fields F-Droid writes that uxdroid does not read, too.
pub fn index(timestamp: u64, apps: &[App]) -> Value {
    let mut packages = serde_json::Map::new();
    for app in apps {
        let mut versions = serde_json::Map::new();
        for version in &app.versions {
            let sha256 = version
                .sha256
                .clone()
                .unwrap_or_else(|| sha256_hex(&version.apk));
            let mut manifest = json!({
                "versionName": version.version_name,
                "versionCode": version.version_code,
                "usesSdk": {"minSdkVersion": version.min_sdk, "targetSdkVersion": 34},
                "usesPermission": [{"name": "android.permission.INTERNET"}],
            });
            if let Some(max) = version.max_sdk {
                manifest["maxSdkVersion"] = json!(max);
            }
            if !version.nativecode.is_empty() {
                manifest["nativecode"] = json!(version.nativecode);
            }
            if let Some(signer) = &version.signer {
                manifest["signer"] = json!({"sha256": signer});
            }
            let mut entry = json!({
                "added": app.last_updated,
                "file": {
                    "name": version.file,
                    "sha256": sha256,
                    "size": version.size.unwrap_or(version.apk.len() as u64),
                },
                "src": {"name": format!("/{}_src.tar.gz", app.package), "sha256": "00", "size": 1},
                "manifest": manifest,
                "whatsNew": {"en-US": "Fixes"},
            });
            if !version.release_channels.is_empty() {
                entry["releaseChannels"] = json!(version.release_channels);
            }
            versions.insert(sha256_hex(&version.apk), entry);
        }
        packages.insert(
            app.package.clone(),
            json!({
                "metadata": {
                    "added": app.last_updated,
                    "categories": ["Writing"],
                    "lastUpdated": app.last_updated,
                    "license": "Apache-2.0",
                    "name": {"en-US": app.name, "de": format!("{} (de)", app.name)},
                    "summary": {"en-US": app.summary},
                    "description": {"en-US": "A long description nobody reads."},
                },
                "versions": versions,
            }),
        );
    }
    json!({
        "repo": {
            "name": {"en-US": "Test repository"},
            "address": "http://localhost/repo",
            "timestamp": timestamp,
        },
        "packages": packages,
    })
}

fn file_ref(name: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "sha256": sha256_hex(bytes), "size": bytes.len()})
}

/// `entry.json` naming `index` and each diff, given as the timestamp it applies to and its bytes.
pub fn entry(timestamp: u64, index: &[u8], diffs: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut named = serde_json::Map::new();
    for (from, diff) in diffs {
        named.insert(
            from.to_string(),
            file_ref(&format!("/diff/{from}.json"), diff),
        );
    }
    serde_json::to_vec(&json!({
        "timestamp": timestamp,
        "version": 30000,
        "maxAge": 14,
        "index": file_ref("/index-v2.json", index),
        "diffs": named,
    }))
    .unwrap()
}

/// `entry.jar`: `entry.json` with a JAR signature by `key`, as apksigner signs it.
pub fn entry_jar(entry: &[u8], key: &Key) -> Vec<u8> {
    Apk::new()
        .file("entry.json", entry)
        .with_v1_signature(key, None)
        .bytes()
}

/// Writes a repository of `apps` signed by `key` into `dir`: `entry.jar`, `index-v2.json`, the
/// diffs and every APK. Returns the bytes of `index-v2.json`.
pub fn write_repo(
    dir: &Path,
    timestamp: u64,
    apps: &[App],
    diffs: &[(u64, Vec<u8>)],
    key: &Key,
) -> Vec<u8> {
    std::fs::create_dir_all(dir.join("diff")).unwrap();
    let index = serde_json::to_vec(&index(timestamp, apps)).unwrap();
    std::fs::write(dir.join("index-v2.json"), &index).unwrap();
    for (from, diff) in diffs {
        std::fs::write(dir.join(format!("diff/{from}.json")), diff).unwrap();
    }
    std::fs::write(
        dir.join("entry.jar"),
        entry_jar(&entry(timestamp, &index, diffs), key),
    )
    .unwrap();
    for app in apps {
        for version in &app.versions {
            if let Some(name) = version.file.strip_prefix('/')
                && !name.contains("..")
                && !name.contains('/')
            {
                std::fs::write(dir.join(name), &version.apk).unwrap();
            }
        }
    }
    index
}

/// The JSON merge patch (RFC 7386) that turns `old` into `new`, as F-Droid's diffs are.
pub fn diff(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            let mut patch = serde_json::Map::new();
            for key in old.keys().filter(|key| !new.contains_key(*key)) {
                patch.insert(key.clone(), Value::Null);
            }
            for (key, value) in new {
                match old.get(key) {
                    Some(previous) if previous == value => {}
                    Some(previous) => {
                        patch.insert(key.clone(), diff(previous, value));
                    }
                    None => {
                        patch.insert(key.clone(), value.clone());
                    }
                }
            }
            Value::Object(patch)
        }
        _ => new.clone(),
    }
}
