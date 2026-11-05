//! A library built with the Android NDK against its `libmediandk.so` binds to this crate's table
//! through the native bridge's loader, as an app's does, and finds no codec.
//!
//! The NDK ships in the dev image. Without it this test fails instead of skipping silently.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn clang() -> PathBuf {
    let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run tests in the dev image");
    Path::new(&ndk).join("toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang")
}

#[test]
fn an_ndk_library_finds_no_codec_and_keeps_a_format() {
    let scratch = std::env::temp_dir().join(format!("uxdroid-media-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let library = scratch.join("libmediacheck.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/mediacheck.c");
    let built = Command::new(clang())
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .arg("-lmediandk")
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
    assert!(loaded.needed().iter().any(|name| name == "libmediandk.so"));
    // SAFETY: `media_check` is `int media_check(void)`.
    assert_eq!(unsafe { loaded.call_i32("media_check") }, Some(0));
}
