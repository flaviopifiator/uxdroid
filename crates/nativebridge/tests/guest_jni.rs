//! An arm64 library loaded and called through `NativeBridgeItf`, the way ART's `java_vm_ext.cc`
//! calls it. Needs the engine (`UXDROID_TRANSLATOR`, set in the `translation` service) and the NDK.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use uxdroid_nativebridge::NativeBridgeItf;

fn engine_available() -> bool {
    let available = std::env::var_os("UXDROID_TRANSLATOR").is_some();
    if !available {
        eprintln!("skipped: UXDROID_TRANSLATOR is not set; run in the translation service");
    }
    available
}

/// A directory this test makes and only this user can write: `create` fails on one that exists.
fn private_dir() -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out = std::env::temp_dir().join(format!("uxdroid-nb-{}-{nanos:x}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&out).unwrap();
    out
}

/// The spike's arm64 library, loaded by the bridge's `loadLibrary`.
fn handle() -> *mut c_void {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| {
        let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run in the dev image");
        let clang = Path::new(&ndk)
            .join("toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang++");
        let path = private_dir().join("libspike.so");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridge/tests/fixtures/arm64/spike.cpp");
        let status = Command::new(clang)
            .args(["-O2", "-fPIC", "-shared", "-static-libstdc++"])
            .arg(source)
            .args(["-llog", "-landroid", "-o"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a C string, and RTLD_NOW as ART passes it.
        let handle = unsafe { (NativeBridgeItf.load_library)(path.as_ptr(), 2) };
        assert!(!handle.is_null());
        handle as usize
    }) as *mut c_void
}

#[test]
fn natives_found_by_name_take_the_shorty_art_passes_without_a_length() {
    if !engine_available() {
        return;
    }
    // ART's `FindSymbolWithNativeBridge` passes the shorty as a C string with length 0.
    let name = c"Java_dev_uxdroid_Spike_scale";
    // SAFETY: a handle from `load_library`, C strings, and the regular JNI call type.
    let trampoline = unsafe {
        (NativeBridgeItf.get_trampoline_with_jni_call_type)(
            handle(),
            name.as_ptr(),
            c"JJI".as_ptr(),
            0,
            0,
        )
    };
    assert!(!trampoline.is_null(), "no trampoline for {name:?}");
    // SAFETY: the C signature of shorty JJI; the method never touches its env.
    let scale: extern "C" fn(usize, usize, i64, i32) -> i64 =
        unsafe { std::mem::transmute(trampoline) };
    assert_eq!(scale(0, 0, 6_000_000_000, 7), 42_000_000_000);
}
