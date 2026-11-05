//! APK signature verification: v2 when the APK has it, v1 otherwise, and damage of either refused.

use std::path::PathBuf;

use uxdroid_apk::{Error, Scheme, verify_signature};
use uxdroid_testkit::{Apk, Key, dex, sample_manifest};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("signature-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn app() -> Apk {
    Apk::new()
        .manifest(&sample_manifest())
        .file("classes.dex", &dex(3, 7))
        .native_lib("x86_64", "liba.so")
}

fn verify(name: &str, bytes: &[u8]) -> Result<uxdroid_apk::Signature, Error> {
    let path = scratch(name).join("app.apk");
    std::fs::write(&path, bytes).unwrap();
    verify_signature(&path)
}

fn reason(result: Result<uxdroid_apk::Signature, Error>) -> String {
    match result {
        Err(Error::BadSignature(reason)) => reason,
        other => panic!("expected a bad signature, got {other:?}"),
    }
}

#[test]
fn a_v2_signature_verifies_and_names_its_certificate() {
    let key = Key::test();
    let signature = verify("v2", &app().signed_bytes(&key)).unwrap();
    assert_eq!(signature.scheme, Scheme::V2);
    assert_eq!(signature.certificates, [key.certificate().to_vec()]);
    assert_eq!(signature.fingerprints(), [key.fingerprint()]);
}

#[test]
fn a_v1_signature_verifies_when_it_is_the_only_one() {
    let key = Key::other();
    let apk = app().with_v1_signature(&key, None);
    let signature = verify("v1", &apk.bytes()).unwrap();
    assert_eq!(signature.scheme, Scheme::V1);
    assert_eq!(signature.fingerprints(), [key.fingerprint()]);
}

#[test]
fn an_unsigned_apk_is_named_as_such() {
    assert!(matches!(
        verify("unsigned", &app().bytes()),
        Err(Error::Unsigned)
    ));
    assert_eq!(
        Error::Unsigned.to_string(),
        "it is not signed, and Android installs only signed APKs"
    );
}

#[test]
fn a_changed_entry_breaks_a_v2_signature() {
    let mut bytes = app().signed_bytes(&Key::test());
    // The first entry is the manifest, stored after its 30-byte local header and name.
    bytes[60] ^= 1;
    assert_eq!(
        reason(verify("v2-changed", &bytes)),
        "its contents do not match their signed digest, so it was changed after signing"
    );
}

#[test]
fn a_changed_signature_does_not_verify() {
    let unsigned = app().bytes();
    let mut bytes = app().signed_bytes(&Key::test());
    // The signature is the last thing before the public key at the end of the signer.
    let footer = bytes.len() - (unsigned.len() - central_directory(&unsigned)) - 24;
    let at = footer - 91 - 8 - 10;
    bytes[at] ^= 0x40;
    let error = verify("v2-signature", &bytes).unwrap_err();
    assert!(matches!(error, Error::BadSignature(_)), "{error:?}");
}

fn central_directory(zip: &[u8]) -> usize {
    let end = (0..zip.len() - 21)
        .rev()
        .find(|at| zip[*at..*at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .unwrap();
    u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize
}

#[test]
fn a_changed_entry_breaks_a_v1_signature() {
    let apk = app()
        .file("assets/data", b"original")
        .with_v1_signature(&Key::test(), None)
        .replace("assets/data", b"changed");
    assert_eq!(
        reason(verify("v1-changed", &apk.bytes())),
        "assets/data does not match its signed digest, so it was changed after signing"
    );
}

#[test]
fn a_v1_entry_the_manifest_does_not_list_is_refused() {
    let apk = app()
        .with_v1_signature(&Key::test(), None)
        .file("assets/added", b"after signing");
    assert_eq!(
        reason(verify("v1-added", &apk.bytes())),
        "assets/added is not signed"
    );
}

#[test]
fn a_v1_signature_that_says_v2_was_removed_is_refused() {
    let apk = app().with_v1_signature(&Key::test(), Some("2"));
    assert_eq!(
        reason(verify("stripped", &apk.bytes())),
        "it says it was also signed with scheme v2 or v3, and that signature was removed"
    );
}

#[test]
fn damage_anywhere_in_the_signing_block_fails_without_panicking() {
    let unsigned = app().bytes();
    let signed = app().signed_bytes(&Key::test());
    let start = central_directory(&unsigned);
    let end = central_directory(&signed);
    let path = scratch("damage").join("app.apk");
    for at in start..end {
        for value in [0x00, 0xff, 0x80] {
            let mut bytes = signed.clone();
            if bytes[at] == value {
                continue;
            }
            bytes[at] = value;
            std::fs::write(&path, &bytes).unwrap();
            // Bytes that make the block unrecognizable leave an unsigned APK; none may verify.
            assert!(
                verify_signature(&path).is_err(),
                "byte {at} set to {value:#x}"
            );
        }
    }
    for length in [0, 1, 21, start, end - 1, signed.len() - 1] {
        std::fs::write(&path, &signed[..length]).unwrap();
        assert!(verify_signature(&path).is_err(), "truncated to {length}");
    }
}

#[test]
fn a_signing_block_pair_length_near_the_largest_integer_is_refused() {
    let start = central_directory(&app().bytes());
    let mut signed = app().signed_bytes(&Key::test());
    // The first pair's length follows the block's leading size.
    signed[start + 8..start + 16].copy_from_slice(&0xffff_ffff_ffff_fff8_u64.to_le_bytes());

    let reason = reason(verify("huge-pair", &signed));

    assert!(reason.contains("bad size"), "{reason}");
}
