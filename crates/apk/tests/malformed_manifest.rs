//! An APK is untrusted input: damaged manifests must fail with an error, never a panic or a hang.

use std::time::{Duration, Instant};

use uxdroid_apk::parse_manifest;
use uxdroid_testkit::{ATTR_NAME, Axml, Value, android, plain, sample_manifest};

#[test]
fn sample_manifest_parses() {
    assert!(parse_manifest(&sample_manifest().encode()).is_ok());
}

#[test]
fn truncation_at_any_length_never_panics() {
    let bytes = sample_manifest().encode();

    for length in 0..bytes.len() {
        let _ = parse_manifest(&bytes[..length]);
    }
}

#[test]
fn corrupting_any_byte_never_panics() {
    let bytes = sample_manifest().encode();

    for position in 0..bytes.len() {
        for value in [0x00, 0x7F, 0x80, 0xFF] {
            let mut damaged = bytes.clone();
            damaged[position] = value;
            let _ = parse_manifest(&damaged);
        }
    }
}

/// Reusing one long string many times must not cost its length on every lookup.
#[test]
fn repeated_long_strings_parse_in_linear_time() {
    const REPEATS: usize = 500;
    let long: &'static str = "x".repeat(256 * 1024).leak();

    let mut root = vec![plain("package", Value::Str("com.example.app"))];
    root.extend((0..REPEATS).map(|_| plain(long, Value::Int(0))));
    let mut manifest = Axml::new().utf16().start("manifest", &root);
    for _ in 0..REPEATS {
        manifest = manifest.start(long, &[]).end(long);
        manifest = manifest
            .start("activity", &[android("name", ATTR_NAME, Value::Str(long))])
            .end("activity");
    }
    let bytes = manifest.end("manifest").encode();

    let start = Instant::now();
    let parsed = parse_manifest(&bytes).unwrap();

    assert_eq!(parsed.package, "com.example.app");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}
