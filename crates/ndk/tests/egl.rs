//! A library built with the Android NDK against its `libEGL.so` and `libGLESv3.so` binds to this
//! crate's tables through the native bridge's loader, as an app's does, and draws on the host's
//! Mesa, as Flutter's Impeller does.
//!
//! The NDK and Mesa ship in the dev image. Without them these tests fail instead of skipping.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn clang() -> PathBuf {
    let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run tests in the dev image");
    Path::new(&ndk).join("toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang")
}

/// Builds `tests/egl/discard.c`, loads it and calls `function`, an `int function(void)`.
fn call(function: &str) -> Option<i32> {
    let scratch =
        std::env::temp_dir().join(format!("uxdroid-egl-{function}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let library = scratch.join("libdiscard.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/egl/discard.c");
    let built = Command::new(clang())
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .args(["-lEGL", "-lGLESv3"])
        .status()
        .unwrap();
    assert!(
        built.success(),
        "the NDK could not build {}",
        source.display()
    );

    for table in uxdroid_ndk::tables() {
        uxdroid_bridge::table::provide(table);
    }
    let loaded = uxdroid_bridge::load(&library, &[]);
    let _ = std::fs::remove_dir_all(&scratch);
    let loaded = loaded.unwrap();
    // SAFETY: every function `call` is given is `int function(void)` in the fixture.
    unsafe { loaded.call_i32(function) }
}

#[test]
fn discarding_the_default_framebuffer_of_a_multisampled_config_returns() {
    assert_eq!(call("discard_default_framebuffer"), Some(0));
}

#[test]
fn invalidating_the_default_framebuffer_of_a_multisampled_config_returns() {
    assert_eq!(call("invalidate_default_framebuffer"), Some(0));
}

#[test]
fn an_apps_own_framebuffer_is_still_invalidated_through_gl() {
    assert_eq!(call("discard_own_framebuffer"), Some(0));
}
