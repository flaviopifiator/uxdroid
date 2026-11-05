//! uxdroid's store: the sources apps install from, what they offer, and verified downloads of it.
//!
//! The command line (`uxdroid search`, `install <package>`, `update`, `sources`) and the terminal
//! UI (`uxdroid tui`) are thin layers over this crate. A source is anything that can list apps with
//! their releases and serve a release's APK with a size and SHA-256 known in advance: F-Droid
//! repositories today; Google Play and direct URLs are meant to become other kinds of source behind the same
//! [`App`] and [`Release`] model. Installing stays the installer's job: this crate hands it a file
//! whose bytes matched what the source signed, and the installer checks them again as it copies.

pub mod config;
pub mod fdroid;
pub mod fetch;
pub mod search;

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use config::{Config, Kind, unknown_source};
use fdroid::Fdroid;
use fetch::{Fetch, Http};

pub use search::{Hit, Matches, search};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Permissions by source, package and version code.
pub type Permissions = std::collections::BTreeMap<(String, String, u64), Vec<String>>;

/// When an index is fetched again before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// When it is older than the configured interval, or there is none.
    IfStale,
    /// Every time, as `--refresh` asks.
    Always,
}

/// What a release must fit: the ABI apps run with and the SDK level the platform reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub abi: String,
    pub sdk: u32,
}

impl Device {
    /// This machine, with the platform uxdroid runs.
    pub fn host() -> Self {
        Self {
            abi: uxdroid_apk::host_abi().to_string(),
            sdk: uxdroid_apk::PLATFORM_SDK,
        }
    }
}

/// An app as one source offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub source: String,
    pub package: String,
    pub name: String,
    pub summary: String,
    /// When the source last changed the app, in milliseconds since the epoch.
    pub updated: Option<u64>,
    /// Its license as the source gives it, usually an SPDX expression.
    pub license: Option<String>,
    /// Newest first.
    pub releases: Vec<Release>,
}

/// One version of an app, as the source signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version_name: String,
    pub version_code: u64,
    pub url: String,
    /// The APK's SHA-256, lowercase hex.
    pub sha256: String,
    pub size: u64,
    pub min_sdk: u32,
    pub max_sdk: Option<u32>,
    /// The ABIs of its native code; empty when it has none.
    pub abis: Vec<String>,
    /// The SHA-256 of each certificate the source says signs the APK, lowercase hex.
    pub signers: Vec<String>,
    pub added: Option<u64>,
    /// Not a beta or other opt-in channel.
    pub stable: bool,
}

impl Release {
    /// Whether it runs here: no native code or some for this ABI, and an SDK range holding the
    /// platform's.
    pub fn fits(&self, device: &Device) -> bool {
        (self.abis.is_empty() || self.abis.contains(&device.abi))
            && self.min_sdk <= device.sdk
            && self.max_sdk.is_none_or(|max| max >= device.sdk)
    }
}

impl App {
    /// The newest stable release that fits `device`.
    pub fn newest(&self, device: &Device) -> Option<&Release> {
        self.releases
            .iter()
            .find(|release| release.stable && release.fits(device))
    }
}

/// App ids become directory names, so only their documented characters are accepted.
pub fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= 255
        && !package.starts_with('.')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

/// Replaces `path` with `bytes` at once: written beside it, then renamed over it.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let cannot =
        |error: std::io::Error| Error::new(format!("cannot write {}: {error}", path.display()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(cannot)?;
    }
    let name = path
        .file_name()
        .map_or_else(Default::default, |name| name.to_string_lossy());
    let temporary = path.with_file_name(format!(".{name}.new-{}", std::process::id()));
    let result = std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(cannot)
}

/// An exclusive lock on `path`, released when the file closes.
pub(crate) fn lock(path: &Path) -> Result<File, Error> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| Error::new(format!("cannot write {}: {error}", path.display())))?;
    file.lock()
        .map_err(|error| Error::new(format!("cannot lock {}: {error}", path.display())))?;
    Ok(file)
}

/// Where an installed app came from, kept by the installer in `<data>/app/.<package>.source`. Apps
/// installed from a file have none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub source: String,
    pub version_code: u64,
    pub version_name: String,
}

impl Installed {
    pub fn path(data: &Path, package: &str) -> PathBuf {
        data.join("app").join(format!(".{package}.source"))
    }

    /// The record of `package`, if it was installed from a source.
    pub fn read(data: &Path, package: &str) -> Result<Option<Self>, Error> {
        let path = Self::path(data, package);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| Error::new(format!("{} is malformed: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::new(format!(
                "cannot read {}: {error}",
                path.display()
            ))),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).expect("a record serializes");
        bytes.push(b'\n');
        bytes
    }
}

/// An APK downloaded into a file this process created, with the SHA-256 the source gave, already
/// checked. The installer checks it again on its own copy. The file goes when this is dropped.
pub struct Download {
    pub path: PathBuf,
    pub sha256: String,
    _lock: File,
}

impl Drop for Download {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The store of one data directory: `<data>/store` holds its sources and their indexes.
pub struct Store {
    root: PathBuf,
    config: Config,
    fetch: Box<dyn Fetch>,
    progress: Box<dyn Fn(&str)>,
    transfer: Box<dyn Fn(u64, u64) -> bool>,
}

impl Store {
    /// The store of `data`, fetching over HTTP.
    pub fn open(data: &Path) -> Result<Self, Error> {
        Self::open_with(data, Box::new(Http::new()))
    }

    pub fn open_with(data: &Path, fetch: Box<dyn Fetch>) -> Result<Self, Error> {
        let root = data.join("store");
        let config = Config::load(&root.join("sources.json"))?;
        Ok(Self {
            root,
            config,
            fetch,
            progress: Box::new(|_| {}),
            transfer: Box::new(|_, _| true),
        })
    }

    /// Calls `progress` with a line about each long download before it starts.
    pub fn on_progress(&mut self, progress: Box<dyn Fn(&str)>) {
        self.progress = progress;
    }

    /// Calls `transfer` with the bytes of an APK downloaded so far and its size as each part
    /// arrives; when it returns `false`, the download stops and fails as cancelled.
    pub fn on_transfer(&mut self, transfer: Box<dyn Fn(u64, u64) -> bool>) {
        self.transfer = transfer;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Changes the configuration with `change` under the store's lock, from what is on disk then, so
    /// concurrent changes are not lost.
    pub fn change(
        &mut self,
        change: impl FnOnce(&mut Config) -> Result<(), Error>,
    ) -> Result<(), Error> {
        std::fs::create_dir_all(&self.root).map_err(|error| {
            Error::new(format!("cannot write {}: {error}", self.root.display()))
        })?;
        let _lock = lock(&self.root.join(".lock"))?;
        let path = self.root.join("sources.json");
        let mut config = Config::load(&path)?;
        change(&mut config)?;
        config.save(&path)?;
        self.config = config;
        Ok(())
    }

    /// The apps of each enabled source, or of `from` alone, enabled or not. A source that cannot be
    /// read is a note, unless no source could be read.
    pub fn apps(
        &self,
        from: Option<&str>,
        refresh: Refresh,
    ) -> Result<(Vec<App>, Vec<String>), Error> {
        let sources: Vec<_> = match from {
            Some(name) => {
                let source = self
                    .config
                    .source(name)
                    .ok_or_else(|| unknown_source(name))?;
                if !source.enabled {
                    return Err(Error::new(format!(
                        "the source {name} is disabled: uxdroid sources enable {name}"
                    )));
                }
                vec![source]
            }
            None => self
                .config
                .sources
                .iter()
                .filter(|source| source.enabled)
                .collect(),
        };
        if sources.is_empty() {
            return Err(Error::new(
                "no source is enabled: uxdroid sources enable <name> enables one",
            ));
        }
        let mut apps = Vec::new();
        let mut notes = Vec::new();
        let mut errors = Vec::new();
        for source in sources {
            let loaded = match source.kind {
                Kind::Fdroid => Fdroid::new(source.clone(), &self.root).load(
                    self.fetch.as_ref(),
                    refresh,
                    self.config.refresh_after_hours,
                    self.progress.as_ref(),
                ),
            };
            match loaded {
                Ok(loaded) => {
                    apps.extend(loaded.apps);
                    notes.extend(loaded.note);
                }
                Err(error) => errors.push(error),
            }
        }
        if apps.is_empty() && !errors.is_empty() {
            return Err(Error::new(
                errors
                    .iter()
                    .map(|error| error.0.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        notes.extend(errors.into_iter().map(|error| error.0));
        Ok((apps, notes))
    }

    /// The permissions each version of each app of the enabled sources requests, as their cached
    /// indexes list them, by source, package and version code. A version missing from it has none
    /// known, as with a cache kept before uxdroid kept permissions, until its next refresh.
    pub fn permissions(&self) -> Permissions {
        let mut found = Permissions::new();
        for source in self.config.sources.iter().filter(|source| source.enabled) {
            let listed = match source.kind {
                Kind::Fdroid => Fdroid::new(source.clone(), &self.root).permissions(),
            };
            for ((package, code), names) in listed {
                found.insert((source.name.clone(), package, code), names);
            }
        }
        found
    }

    /// Downloads `release` into `<data>/store/downloads`, refusing more bytes than it has or other
    /// contents than its SHA-256.
    pub fn download(&self, app: &App, release: &Release) -> Result<Download, Error> {
        if !valid_package(&app.package) {
            return Err(Error::new(format!(
                "bad package name {}",
                app.package.escape_debug()
            )));
        }
        let directory = self.root.join("downloads");
        std::fs::create_dir_all(&directory).map_err(|error| {
            Error::new(format!("cannot write {}: {error}", directory.display()))
        })?;
        sweep_downloads(&directory);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let path = directory.join(format!(
            "{}-{}-{}-{stamp}.apk",
            app.package,
            release.version_code,
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| Error::new(format!("cannot write {}: {error}", path.display())))?;
        let lock = file
            .try_clone()
            .and_then(|lock| {
                lock.try_lock()
                    .map_err(std::io::Error::other)
                    .map(|()| lock)
            })
            .map_err(|error| Error::new(format!("cannot lock {}: {error}", path.display())))?;
        let download = Download {
            path,
            sha256: release.sha256.clone(),
            _lock: lock,
        };
        let mut hashing = Hashing {
            inner: &mut file,
            hash: Sha256::new(),
            done: 0,
            total: release.size,
            transfer: self.transfer.as_ref(),
            cancelled: false,
        };
        let copied = self.fetch.copy(&release.url, release.size, &mut hashing);
        if hashing.cancelled {
            return Err(Error::new(format!(
                "the download of {} from {} was cancelled",
                app.package, app.source
            )));
        }
        let size = copied.map_err(|error| {
            if error.0.contains("is larger than") {
                Error::new(format!(
                    "the APK of {} from {} is larger than the {} bytes its index gives, so it is refused",
                    app.package, app.source, release.size
                ))
            } else {
                error
            }
        })?;
        let digest: String = hashing
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if size != release.size || digest != release.sha256 {
            return Err(Error::new(format!(
                "the APK of {} from {} does not match its index (SHA-256 {}, {} bytes; the index gives {}, {} bytes), so it is refused",
                app.package, app.source, digest, size, release.sha256, release.size
            )));
        }
        file.sync_all().map_err(|error| {
            Error::new(format!("cannot write {}: {error}", download.path.display()))
        })?;
        Ok(download)
    }
}

/// Removes downloads that no process holds, left by one that died.
fn sweep_downloads(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let free = File::open(&path)
            .map(|file| file.try_lock().is_ok())
            .unwrap_or(false);
        if free {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Writes a download to its file, hashing it and reporting its progress.
struct Hashing<'a> {
    inner: &'a mut File,
    hash: Sha256,
    done: u64,
    total: u64,
    transfer: &'a dyn Fn(u64, u64) -> bool,
    cancelled: bool,
}

impl Write for Hashing<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancelled {
            return Err(std::io::Error::other("cancelled"));
        }
        let written = self.inner.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.done += written as u64;
        if !(self.transfer)(self.done, self.total) {
            self.cancelled = true;
            return Err(std::io::Error::other("cancelled"));
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
