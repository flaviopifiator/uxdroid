//! An APK is untrusted input: damaged manifests must fail with an error, never a panic or a hang.

use uxdroid_apk::parse_manifest;
use uxdroid_testkit::sample_manifest;

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
