//! Reads APK files and plans how to execute them on this machine.

mod der;
mod manifest;
mod signing;
mod splits;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;
use zip::result::ZipError;

pub use manifest::{Manifest, parse_manifest};
pub use signing::{PLATFORM_SDK, Scheme, Signature, verify_signature};
pub use splits::{Host, Kind, Part, Plan, bundle_entries, extract_bundled, plan_splits};

pub(crate) const MANIFEST_ENTRY: &str = "AndroidManifest.xml";
/// Larger manifests do not occur in practice; the limit stops decompression bombs.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
/// The largest real library seen in samples is 54 MiB. The limit stops decompression bombs.
const MAX_NATIVE_LIB_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_NATIVE_LIBS_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
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
    CannotExtract(String),
    /// Neither a v2 or v3 signing block nor a JAR signature.
    Unsigned,
    BadSignature(String),
    /// Split APKs that do not make one app.
    Splits(String),
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
            Error::CannotExtract(reason) => write!(f, "cannot extract native library: {reason}"),
            Error::Unsigned => write!(f, "it is not signed, and Android installs only signed APKs"),
            Error::BadSignature(reason) => write!(f, "its signature does not verify: {reason}"),
            Error::Splits(reason) => f.write_str(reason),
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

/// Identifies an APK's contents from its archive directory, without decompressing anything.
///
/// Every entry's name, size and CRC go in, so two APKs with different code get different values
/// even with the same size and timestamp.
pub fn fingerprint(path: &Path) -> Result<u64, Error> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let file = File::open(path).map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let mut hash = FNV_OFFSET;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|source| Error::InvalidArchive(source.to_string()))?;
        let crc = entry.crc32().to_le_bytes();
        let size = entry.size().to_le_bytes();
        let name = entry
            .name()
            .map_err(|source| Error::InvalidArchive(source.to_string()))?;
        let fields: [&[u8]; 3] = [name.as_bytes(), &crc, &size];
        for byte in fields.into_iter().flatten() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
        }
    }
    Ok(hash)
}

/// Extracts the APK's native libraries for one ABI into `directory`, returning how many.
///
/// Entry names are untrusted: `native_lib_abi` accepts only `lib/<abi>/<file>.so`, and a file name
/// of `..` cannot end in `.so`, so nothing lands outside `directory`.
pub fn extract_native_libs(path: &Path, abi: &str, directory: &Path) -> Result<u32, Error> {
    let file = File::open(path).map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let cannot = |reason: String| Error::CannotExtract(reason);
    // Which of two entries with one name gets extracted depends on the reader; Android's refuses the
    // whole archive, and so does this.
    if archive.has_duplicate_names() {
        return Err(Error::InvalidArchive(
            "it names an entry twice, which Android refuses".to_string(),
        ));
    }

    let names: Vec<String> = archive
        .file_names()
        .map(|name| name.map(|text| text.to_string()))
        .collect::<Result<_, _>>()
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let mut count = 0;
    let mut total = 0;
    for name in names
        .iter()
        .filter(|name| native_lib_abi(name) == Some(abi))
    {
        let Some(file_name) = name.rsplit('/').next().filter(|file| !file.contains('\\')) else {
            return Err(cannot(format!("bad entry name {}", name.escape_debug())));
        };
        let entry = archive
            .by_name(name)
            .map_err(|source| Error::InvalidArchive(source.to_string()))?;
        let mut limited = entry.take(MAX_NATIVE_LIB_BYTES + 1);
        let partial = directory.join(format!("{file_name}.partial"));
        let mut output = File::create(&partial).map_err(|error| cannot(error.to_string()))?;
        let written =
            io::copy(&mut limited, &mut output).map_err(|error| cannot(error.to_string()))?;
        total += written;
        if written > MAX_NATIVE_LIB_BYTES || total > MAX_NATIVE_LIBS_TOTAL_BYTES {
            let _ = std::fs::remove_file(&partial);
            return Err(cannot(format!("{} is too large", name.escape_debug())));
        }
        std::fs::rename(&partial, directory.join(file_name))
            .map_err(|error| cannot(error.to_string()))?;
        count += 1;
    }
    Ok(count)
}

fn read_manifest(archive: &mut ZipArchive<BufReader<File>>) -> Result<Vec<u8>, Error> {
    let entry = archive
        .by_name(MANIFEST_ENTRY)
        .map_err(|source| match source {
            ZipError::FileNotFound => Error::MissingManifest,
            other => Error::InvalidArchive(other.to_string()),
        })?;
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

/// Android loads `classes.dex`, `classes2.dex`, `classes3.dex`, ... from the archive root only.
fn is_dex_entry(name: &str) -> bool {
    name.strip_prefix("classes")
        .and_then(|rest| rest.strip_suffix(".dex"))
        .is_some_and(|number| {
            number.is_empty()
                || (number != "1"
                    && !number.starts_with('0')
                    && number.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

fn native_lib_abi(name: &str) -> Option<&str> {
    let mut parts = name.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("lib"), Some(abi), Some(file), None) if !abi.is_empty() && file.ends_with(".so") => {
            Some(abi)
        }
        _ => None,
    }
}
