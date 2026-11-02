//! Native library extraction handles untrusted archive entry names.

use std::path::PathBuf;

use uxdroid_apk::{extract_native_libs, fingerprint};
use uxdroid_testkit::{Apk, sample_manifest};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("uxdroid-extract-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn extracts_only_the_requested_abi() {
    let dir = scratch("abi");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/x86_64/liba.so", b"host")
        .file("lib/arm64-v8a/libb.so", b"other")
        .file("lib/x86_64/readme.txt", b"not a library")
        .write(&apk);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    assert_eq!(extract_native_libs(&apk, "x86_64", &out).unwrap(), 1);

    assert_eq!(std::fs::read(out.join("liba.so")).unwrap(), b"host");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
}

#[test]
fn entries_cannot_escape_the_directory() {
    let dir = scratch("escape");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/x86_64/../../escaped.so", b"evil")
        .file("lib/x86_64/sub/nested.so", b"evil")
        .file("/lib/x86_64/absolute.so", b"evil")
        .write(&apk);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    assert_eq!(extract_native_libs(&apk, "x86_64", &out).unwrap(), 0);

    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    assert!(!dir.join("escaped.so").exists());
}

#[test]
fn the_fingerprint_follows_contents_and_not_file_metadata() {
    let dir = scratch("fingerprint");
    let write = |name: &str, dex: &[u8]| {
        let apk = dir.join(name);
        Apk::new()
            .manifest(&sample_manifest())
            .file("classes.dex", dex)
            .write(&apk);
        apk
    };
    let first = write("first.apk", b"code one");
    let same = write("same.apk", b"code one");
    let other = write("other.apk", b"code two");

    assert_eq!(fingerprint(&first).unwrap(), fingerprint(&same).unwrap());
    assert_ne!(fingerprint(&first).unwrap(), fingerprint(&other).unwrap());
}
