//! Reads APK files and plans how to execute them on this machine.

mod manifest;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

pub use manifest::{Manifest, parse_manifest};

const MANIFEST_ENTRY: &str = "AndroidManifest.xml";
/// Larger manifests do not occur in practice; the limit stops decompression bombs.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const DEX_HEADER_BYTES: usize = 112;
const DEX_METHOD_COUNT_OFFSET: usize = 88;
const DEX_CLASS_COUNT_OFFSET: usize = 96;

#[derive(Debug)]
pub enum Error {
    Unreadable(PathBuf, io::Error),
    InvalidArchive(String),
    MissingManifest,
    MalformedManifest(&'static str),
    MalformedDex(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unreadable(path, source) => {
                write!(f, "cannot read APK {}: {source}", path.display())
            }
            Error::InvalidArchive(reason) => write!(f, "not a valid APK archive: {reason}"),
            Error::MissingManifest => write!(f, "APK has no {MANIFEST_ENTRY}"),
            Error::MalformedManifest(reason) => write!(f, "malformed manifest: {reason}"),
            Error::MalformedDex(entry) => write!(f, "malformed DEX {entry}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DexTotals {
    pub files: u32,
    pub classes: u64,
    pub methods: u64,
}

#[derive(Debug)]
pub struct Apk {
    pub manifest: Manifest,
    pub dex: DexTotals,
    /// Native library count per ABI.
    pub abis: BTreeMap<String, u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionPath {
    /// DEX compiles to host code and native libraries, if any, load as they are.
    Direct,
    /// Native libraries exist only for these ABIs and need translation.
    NeedsAbiBridge(Vec<String>),
}

impl fmt::Display for ExecutionPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionPath::Direct => write!(f, "direct"),
            ExecutionPath::NeedsAbiBridge(abis) => {
                write!(f, "needs-abi-bridge ({})", abis.join(", "))
            }
        }
    }
}

impl Apk {
    pub fn execution_path(&self, host_abi: &str) -> ExecutionPath {
        if self.abis.is_empty() || self.abis.contains_key(host_abi) {
            ExecutionPath::Direct
        } else {
            ExecutionPath::NeedsAbiBridge(self.abis.keys().cloned().collect())
        }
    }
}

/// The Android ABI name of the machine this binary was built for.
pub fn host_abi() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        other => other,
    }
}

/// Reads the metadata of an APK without decompressing its code.
pub fn inspect(path: &Path) -> Result<Apk, Error> {
    let file = File::open(path).map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;

    let manifest = parse_manifest(&read_manifest(&mut archive)?)?;

    let mut abis = BTreeMap::new();
    let mut dex_entries = Vec::new();
    for name in archive.file_names() {
        let name = name.map_err(|source| Error::InvalidArchive(source.to_string()))?;
        if is_dex_entry(&name) {
            dex_entries.push(name.to_string());
        } else if let Some(abi) = native_lib_abi(&name) {
            *abis.entry(abi.to_string()).or_insert(0) += 1;
        }
    }

    let mut dex = DexTotals::default();
    for entry in dex_entries {
        let (classes, methods) = read_dex_counts(&mut archive, &entry)?;
        dex.files += 1;
        dex.classes += u64::from(classes);
        dex.methods += u64::from(methods);
    }

    Ok(Apk {
        manifest,
        dex,
        abis,
    })
}

fn read_manifest(archive: &mut ZipArchive<BufReader<File>>) -> Result<Vec<u8>, Error> {
    let entry = archive
        .by_name(MANIFEST_ENTRY)
        .map_err(|_| Error::MissingManifest)?;
    let mut bytes = Vec::new();
    entry
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(Error::MalformedManifest("too large"));
    }
    Ok(bytes)
}

/// Reads only the fixed-size header, so large DEX files cost nothing to inspect.
fn read_dex_counts(
    archive: &mut ZipArchive<BufReader<File>>,
    name: &str,
) -> Result<(u32, u32), Error> {
    let malformed = || Error::MalformedDex(name.to_string());
    let mut entry = archive.by_name(name).map_err(|_| malformed())?;
    let mut header = [0u8; DEX_HEADER_BYTES];
    entry.read_exact(&mut header).map_err(|_| malformed())?;

    if &header[..4] != b"dex\n" || header[7] != 0 {
        return Err(malformed());
    }
    let count_at = |offset: usize| {
        u32::from_le_bytes([
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ])
    };
    Ok((
        count_at(DEX_CLASS_COUNT_OFFSET),
        count_at(DEX_METHOD_COUNT_OFFSET),
    ))
}

/// Android loads `classes.dex`, `classes2.dex`, ... from the archive root only.
fn is_dex_entry(name: &str) -> bool {
    name.strip_prefix("classes")
        .and_then(|rest| rest.strip_suffix(".dex"))
        .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()))
}

fn native_lib_abi(name: &str) -> Option<&str> {
    let mut parts = name.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("lib"), Some(abi), Some(file), None) if file.ends_with(".so") => Some(abi),
        _ => None,
    }
}
