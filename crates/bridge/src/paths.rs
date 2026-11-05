//! Android's file system layout for app libraries.
//!
//! Native code names files of the system partition by absolute path, such as Skia reading
//! `/system/etc/fonts.xml`. Here the system partition is the runtime root that `ANDROID_ROOT`
//! names, as it is for the framework, so the functions that take a path move `/system` there.
//! Everything else reaches glibc unchanged; these functions are on the audited list otherwise.

use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::OnceLock;

const SYSTEM: &[u8] = b"/system";

/// `path` inside `root` when it names the system partition, or `None` when it stays as it is.
fn android_path(path: &[u8], root: &[u8]) -> Option<Vec<u8>> {
    let rest = path.strip_prefix(SYSTEM)?;
    if !(rest.is_empty() || rest.starts_with(b"/")) {
        return None;
    }
    let mut moved = root.to_vec();
    moved.extend_from_slice(rest);
    Some(moved)
}

/// The runtime root, when it is a usable absolute path other than `/system` itself.
fn system_root() -> Option<&'static [u8]> {
    static ROOT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::var_os("ANDROID_ROOT")?.into_encoded_bytes();
        let root = root.strip_suffix(b"/").unwrap_or(&root).to_vec();
        (root.starts_with(b"/") && root != SYSTEM && !root.contains(&0)).then_some(root)
    })
    .as_deref()
}

/// Runs `call` with `path`, or with its place under the runtime root.
///
/// # Safety
///
/// `path` must be null or a valid C string.
unsafe fn with_path<T>(path: *const c_char, call: impl FnOnce(*const c_char) -> T) -> T {
    let moved = match (path.is_null(), system_root()) {
        (false, Some(root)) => {
            // SAFETY: the caller guarantees a C string.
            let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
            android_path(bytes, root).and_then(|moved| CString::new(moved).ok())
        }
        _ => None,
    };
    call(moved.as_ref().map_or(path, |moved| moved.as_ptr()))
}

// The functions below keep their libc signatures; each only changes the path it passes on.
// SAFETY (all): callers follow the libc contract of the function they call, and `with_path`
// passes either their own pointer or a C string that lives until glibc returns.

unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: libc::mode_t) -> c_int {
    unsafe { with_path(path, |path| libc::open(path, flags, mode)) }
}

unsafe extern "C" fn open_2(path: *const c_char, flags: c_int) -> c_int {
    unsafe { with_path(path, |path| libc::open(path, flags)) }
}

unsafe extern "C" fn openat(
    directory: c_int,
    path: *const c_char,
    flags: c_int,
    mode: libc::mode_t,
) -> c_int {
    unsafe { with_path(path, |path| libc::openat(directory, path, flags, mode)) }
}

unsafe extern "C" fn openat_2(directory: c_int, path: *const c_char, flags: c_int) -> c_int {
    unsafe { with_path(path, |path| libc::openat(directory, path, flags)) }
}

unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE {
    unsafe { with_path(path, |path| libc::fopen(path, mode)) }
}

unsafe extern "C" fn stat(path: *const c_char, buffer: *mut libc::stat) -> c_int {
    unsafe { with_path(path, |path| libc::stat(path, buffer)) }
}

unsafe extern "C" fn lstat(path: *const c_char, buffer: *mut libc::stat) -> c_int {
    unsafe { with_path(path, |path| libc::lstat(path, buffer)) }
}

unsafe extern "C" fn fstatat(
    directory: c_int,
    path: *const c_char,
    buffer: *mut libc::stat,
    flags: c_int,
) -> c_int {
    unsafe { with_path(path, |path| libc::fstatat(directory, path, buffer, flags)) }
}

unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    unsafe { with_path(path, |path| libc::access(path, mode)) }
}

unsafe extern "C" fn faccessat(
    directory: c_int,
    path: *const c_char,
    mode: c_int,
    flags: c_int,
) -> c_int {
    unsafe { with_path(path, |path| libc::faccessat(directory, path, mode, flags)) }
}

unsafe extern "C" fn opendir(path: *const c_char) -> *mut libc::DIR {
    unsafe { with_path(path, |path| libc::opendir(path)) }
}

unsafe extern "C" fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char {
    unsafe { with_path(path, |path| libc::realpath(path, resolved)) }
}

unsafe extern "C" fn readlink(path: *const c_char, buffer: *mut c_char, size: usize) -> isize {
    unsafe { with_path(path, |path| libc::readlink(path, buffer, size)) }
}

/// Bionic names and their shims. The `64` names are the same functions on 64-bit Android.
pub(crate) const PATH_FUNCTIONS: &[(&str, *const ())] = &[
    ("open", open as *const ()),
    ("open64", open as *const ()),
    ("__open_2", open_2 as *const ()),
    ("openat", openat as *const ()),
    ("openat64", openat as *const ()),
    ("__openat_2", openat_2 as *const ()),
    ("fopen", fopen as *const ()),
    ("fopen64", fopen as *const ()),
    ("stat", stat as *const ()),
    ("stat64", stat as *const ()),
    ("lstat", lstat as *const ()),
    ("lstat64", lstat as *const ()),
    ("fstatat", fstatat as *const ()),
    ("fstatat64", fstatat as *const ()),
    ("access", access as *const ()),
    ("faccessat", faccessat as *const ()),
    ("opendir", opendir as *const ()),
    ("realpath", realpath as *const ()),
    ("readlink", readlink as *const ()),
];

#[cfg(test)]
mod tests {
    use super::android_path;

    #[test]
    fn the_system_partition_moves_under_the_runtime_root() {
        assert_eq!(
            android_path(b"/system/etc/fonts.xml", b"/runtime/art").as_deref(),
            Some(&b"/runtime/art/etc/fonts.xml"[..])
        );
        assert_eq!(
            android_path(b"/system", b"/runtime/art").as_deref(),
            Some(&b"/runtime/art"[..])
        );
    }

    #[test]
    fn other_paths_stay() {
        for path in [
            &b"/systemd/x"[..],
            b"/data/system/x",
            b"system/etc/fonts.xml",
            b"/vendor/etc/x",
            b"",
        ] {
            assert_eq!(android_path(path, b"/runtime/art"), None, "{path:?}");
        }
    }
}
