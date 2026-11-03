//! One running instance per app, and the control socket through which commands reach it.
//!
//! A started app holds an exclusive `flock` on `<ANDROID_DATA>/app/.<package>.running` for its whole
//! life. The kernel drops it when the process ends, however it ends, so a crash never leaves a stale
//! lock. A second launch over the same data directory finds the lock held and hands its intent to the
//! running app through its control socket instead of starting a second process over the same files.
//!
//! The socket is `<control dir>/<package>.sock`, mode 0600, in a directory only the user can enter:
//! `$XDG_RUNTIME_DIR/uxdroid`, or `/tmp/uxdroid-<uid>` without one. Each connection's peer must be the
//! same user. The protocol is in `protocol`; `server` serves it and `client` calls it.

pub mod client;
mod java;
pub mod protocol;
#[cfg(test)]
mod protocol_tests;
pub mod server;
#[cfg(test)]
mod server_tests;

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

pub use java::register_natives;

/// The longest path a Unix socket address holds, its terminating zero excluded.
const MAX_SOCKET_PATH: usize = 107;

/// Package names become file names, so only their documented characters are accepted.
pub fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('.')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The user's control directory: `$XDG_RUNTIME_DIR/uxdroid`, else `/tmp/uxdroid-<uid>`, made if missing.
pub fn control_dir() -> Result<PathBuf, String> {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| Path::new(dir).is_absolute()) {
        Some(runtime) => control_dir_in(Path::new(&runtime)),
        None => private_dir(&std::env::temp_dir().join(format!("uxdroid-{}", euid()))),
    }
}

/// `<base>/uxdroid`, made with mode 0700 if missing. An existing one must be a directory of this user
/// that no one else can enter, or it is refused: anyone who can enter it could reach the sockets.
pub fn control_dir_in(base: &Path) -> Result<PathBuf, String> {
    private_dir(&base.join("uxdroid"))
}

fn private_dir(dir: &Path) -> Result<PathBuf, String> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot make {}: {error}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != euid() || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} belongs to another user or other users can enter it; make it yours with mode 0700",
            dir.display()
        ));
    }
    Ok(dir.to_path_buf())
}

pub fn socket_path(dir: &Path, package: &str) -> Result<PathBuf, String> {
    if !valid_package(package) {
        return Err(format!("{} is not a package name", package.escape_debug()));
    }
    let path = dir.join(format!("{package}.sock"));
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!(
            "{} is longer than a socket address can be",
            path.display()
        ));
    }
    Ok(path)
}

/// The instance lock of `package` over the data directory `data`.
pub fn instance_lock(data: &Path, package: &str) -> PathBuf {
    data.join("app").join(format!(".{package}.running"))
}

/// Takes the exclusive lock on `path`, making the file and its directory if missing. `None` when
/// another open file holds it. The lock lasts until the returned file closes; it is never inherited
/// by programs the app runs, because the file closes on exec.
pub fn claim_instance(path: &Path) -> std::io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}
