//! Signs synthetic APKs as apksigner does, with fixed EC P-256 keys, so tests need no SDK and get
//! the same bytes every run.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};

use crate::Apk;

/// A signing key and its self-signed certificate.
pub struct Key {
    signing: SigningKey,
    certificate: Vec<u8>,
    public_key: Vec<u8>,
}

impl Key {
    /// The key most tests sign with.
    pub fn test() -> Self {
        Self::from_seed(0x11, "uxdroid test")
    }

    /// A second key, for an app signed by someone else.
    pub fn other() -> Self {
        Self::from_seed(0x22, "uxdroid other")
    }

    fn from_seed(seed: u8, name: &str) -> Self {
        let signing = SigningKey::from_bytes(&[seed; 32].into()).unwrap();
        let point = signing.verifying_key().to_encoded_point(false);
        let public_key = der(
            0x30,
            &[
                der(
                    0x30,
                    &[
                        oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]),
                        oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07]),
                    ]
                    .concat(),
                ),
                der(0x03, &[&[0u8][..], point.as_bytes()].concat()),
            ]
            .concat(),
        );
        let ecdsa_sha256 = der(
            0x30,
            &oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
        );
        let name = der(
            0x30,
            &der(
                0x31,
                &der(
                    0x30,
                    &[oid(&[0x55, 0x04, 0x03]), der(0x0c, name.as_bytes())].concat(),
                ),
            ),
        );
        let tbs = der(
            0x30,
            &[
                der(0xa0, &der(0x02, &[2])),
                der(0x02, &[seed]),
                ecdsa_sha256.clone(),
                name.clone(),
                der(
                    0x30,
                    &[der(0x17, b"260101000000Z"), der(0x17, b"360101000000Z")].concat(),
                ),
                name,
                public_key.clone(),
            ]
            .concat(),
        );
        let signature: Signature = signing.sign(&tbs);
        let certificate = der(
            0x30,
            &[
                tbs,
                ecdsa_sha256,
                der(0x03, &[&[0u8][..], &signature_der(&signature)].concat()),
            ]
            .concat(),
        );
        Self {
            signing,
            certificate,
            public_key,
        }
    }

    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// The certificate's SHA-256 in hex, as `apksigner verify --print-certs` prints it.
    pub fn fingerprint(&self) -> String {
        Sha256::digest(&self.certificate)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        signature_der(&self.signing.sign(message))
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(content);
    out
}

fn oid(encoded: &[u8]) -> Vec<u8> {
    der(0x06, encoded)
}

fn unsigned_integer(bytes: &[u8]) -> Vec<u8> {
    let trimmed: Vec<u8> = bytes
        .iter()
        .copied()
        .skip_while(|byte| *byte == 0)
        .collect();
    let mut content = if trimmed.first().is_some_and(|byte| byte & 0x80 != 0) {
        vec![0]
    } else {
        Vec::new()
    };
    content.extend(if trimmed.is_empty() { vec![0] } else { trimmed });
    der(0x02, &content)
}

fn signature_der(signature: &Signature) -> Vec<u8> {
    let (r, s) = signature.split_bytes();
    der(0x30, &[unsigned_integer(&r), unsigned_integer(&s)].concat())
}

fn prefixed(content: &[u8]) -> Vec<u8> {
    let mut out = (content.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(content);
    out
}

fn sequence(items: &[Vec<u8>]) -> Vec<u8> {
    prefixed(
        &items
            .iter()
            .map(|item| prefixed(item))
            .collect::<Vec<_>>()
            .concat(),
    )
}

const ECDSA_SHA256: u32 = 0x0201;
const BLOCK_V2: u32 = 0x7109_871a;
pub const BLOCK_V3: u32 = 0xf053_68c0;
pub const BLOCK_V31: u32 = 0x1b93_ad61;
const PROOF_OF_ROTATION: u32 = 0x3ba0_6f8c;
/// A past signer's capability: the data an app installed with it keeps is trusted.
pub const INSTALLED_DATA: u32 = 1;

/// Where the archive's end record and central directory are, and the digest a signer signs.
fn layout(zip: &[u8]) -> (usize, usize, Vec<u8>) {
    let end = (0..=zip.len() - 22)
        .rev()
        .find(|at| zip[*at..*at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .unwrap();
    let central_directory =
        u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
    let digest = content_digest(
        &zip[..central_directory],
        &zip[central_directory..end],
        &zip[end..],
        central_directory as u32,
    );
    (end, central_directory, digest)
}

/// Inserts an APK Signing Block with one v2 signer before the central directory.
pub(crate) fn sign_v2(zip: &[u8], key: &Key) -> Vec<u8> {
    let (_, _, digest) = layout(zip);
    let mut algorithm_digest = ECDSA_SHA256.to_le_bytes().to_vec();
    algorithm_digest.extend(prefixed(&digest));
    let signed_data = [
        sequence(&[algorithm_digest]),
        sequence(std::slice::from_ref(&key.certificate)),
        sequence(&[]),
    ]
    .concat();
    let mut signature = ECDSA_SHA256.to_le_bytes().to_vec();
    signature.extend(prefixed(&key.sign(&signed_data)));
    let signer = [
        prefixed(&signed_data),
        sequence(&[signature]),
        prefixed(&key.public_key),
    ]
    .concat();
    insert_block(zip, BLOCK_V2, &sequence(&[signer]))
}

/// One past signer of a rotated key: its key, the capabilities it keeps, and, to forge a lineage,
/// another key that signs its successor's node instead of it.
pub struct Past<'a> {
    pub key: &'a Key,
    pub flags: u32,
    pub forged_by: Option<&'a Key>,
}

/// A proof-of-rotation lineage, as apksigner writes it: version 1, then a node per certificate from
/// the oldest to `current`, each signed by the key before it.
fn lineage(past: &[Past], current: &Key) -> Vec<u8> {
    let mut out = 1u32.to_le_bytes().to_vec();
    let certificates: Vec<(&Key, u32)> = past
        .iter()
        .map(|past| (past.key, past.flags))
        .chain([(current, 0)])
        .collect();
    for (index, (key, flags)) in certificates.iter().enumerate() {
        // The algorithm that signs this node, by the key before it; none for the first.
        let signed_with = if index == 0 { 0 } else { ECDSA_SHA256 };
        let signed_data = [
            prefixed(&key.certificate),
            signed_with.to_le_bytes().to_vec(),
        ]
        .concat();
        let signature = match index {
            0 => Vec::new(),
            _ => past[index - 1]
                .forged_by
                .unwrap_or(past[index - 1].key)
                .sign(&signed_data),
        };
        let next = if index + 1 < certificates.len() {
            ECDSA_SHA256
        } else {
            0
        };
        let node = [
            prefixed(&signed_data),
            flags.to_le_bytes().to_vec(),
            next.to_le_bytes().to_vec(),
            prefixed(&signature),
        ]
        .concat();
        out.extend(prefixed(&node));
    }
    out
}

/// Inserts an APK Signing Block with one v3 (`BLOCK_V3`) or v3.1 (`BLOCK_V31`) signer for SDK 24
/// and later, whose key rotated from `past`, oldest first, to `last` (normally `key` itself), when
/// `past` is not empty.
pub(crate) fn sign_v3(zip: &[u8], key: &Key, past: &[Past], last: &Key, block: u32) -> Vec<u8> {
    let (_, _, digest) = layout(zip);
    let mut algorithm_digest = ECDSA_SHA256.to_le_bytes().to_vec();
    algorithm_digest.extend(prefixed(&digest));
    let (min, max) = (24u32, i32::MAX as u32);
    let attributes: Vec<Vec<u8>> = if past.is_empty() {
        Vec::new()
    } else {
        vec![
            [
                PROOF_OF_ROTATION.to_le_bytes().to_vec(),
                lineage(past, last),
            ]
            .concat(),
        ]
    };
    let signed_data = [
        sequence(&[algorithm_digest]),
        sequence(std::slice::from_ref(&key.certificate)),
        min.to_le_bytes().to_vec(),
        max.to_le_bytes().to_vec(),
        sequence(&attributes),
    ]
    .concat();
    let mut signature = ECDSA_SHA256.to_le_bytes().to_vec();
    signature.extend(prefixed(&key.sign(&signed_data)));
    let signer = [
        prefixed(&signed_data),
        min.to_le_bytes().to_vec(),
        max.to_le_bytes().to_vec(),
        sequence(&[signature]),
        prefixed(&key.public_key),
    ]
    .concat();
    insert_block(zip, block, &sequence(&[signer]))
}

/// Inserts an APK Signing Block holding the one pair `id`, `value` before the central directory.
fn insert_block(zip: &[u8], id: u32, value: &[u8]) -> Vec<u8> {
    let (end, central_directory, _) = layout(zip);
    let value = [id.to_le_bytes().to_vec(), value.to_vec()].concat();
    let mut pairs = ((value.len()) as u64).to_le_bytes().to_vec();
    pairs.extend(value);
    let size = (pairs.len() + 8 + 16) as u64;
    let block = [
        size.to_le_bytes().to_vec(),
        pairs,
        size.to_le_bytes().to_vec(),
        b"APK Sig Block 42".to_vec(),
    ]
    .concat();
    let mut out = zip[..central_directory].to_vec();
    out.extend(&block);
    out.extend(&zip[central_directory..end]);
    let mut record = zip[end..].to_vec();
    record[16..20].copy_from_slice(&((central_directory + block.len()) as u32).to_le_bytes());
    out.extend(record);
    out
}

fn content_digest(entries: &[u8], directory: &[u8], end: &[u8], block_offset: u32) -> Vec<u8> {
    let mut end = end.to_vec();
    end[16..20].copy_from_slice(&block_offset.to_le_bytes());
    let chunks: Vec<&[u8]> = [entries, directory, &end]
        .into_iter()
        .flat_map(|section| section.chunks(1024 * 1024))
        .collect();
    let mut top = Sha256::new();
    top.update([0x5a]);
    top.update((chunks.len() as u32).to_le_bytes());
    for chunk in chunks {
        let mut digest = Sha256::new();
        digest.update([0xa5]);
        digest.update((chunk.len() as u32).to_le_bytes());
        digest.update(chunk);
        top.update(digest.finalize());
    }
    top.finalize().to_vec()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .fold(0u32, |word, byte| (word << 8) | u32::from(*byte))
            << (8 * (3 - chunk.len()));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[(word >> (18 - 6 * index)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Adds `META-INF/MANIFEST.MF`, `CERT.SF` and `CERT.EC`, as jarsigner writes them.
pub(crate) fn sign_v1(apk: Apk, key: &Key, apk_signed: Option<&str>) -> Apk {
    let digest = |bytes: &[u8]| base64(&Sha256::digest(bytes));
    let mut manifest = "Manifest-Version: 1.0\r\nCreated-By: uxdroid-testkit\r\n\r\n".to_string();
    let mut sections = Vec::new();
    for (name, bytes) in &apk.entries {
        let section = format!("Name: {name}\r\nSHA-256-Digest: {}\r\n\r\n", digest(bytes));
        manifest.push_str(&section);
        sections.push((name.clone(), section));
    }
    let mut signature_file = format!(
        "Signature-Version: 1.0\r\nCreated-By: uxdroid-testkit\r\nSHA-256-Digest-Manifest: {}\r\n",
        digest(manifest.as_bytes())
    );
    if let Some(schemes) = apk_signed {
        signature_file.push_str(&format!("X-Android-APK-Signed: {schemes}\r\n"));
    }
    signature_file.push_str("\r\n");
    for (name, section) in sections {
        signature_file.push_str(&format!(
            "Name: {name}\r\nSHA-256-Digest: {}\r\n\r\n",
            digest(section.as_bytes())
        ));
    }

    let certificate = &key.certificate;
    // The issuer and serial number, read back from the certificate this module wrote.
    let (tbs, _) = element(content(certificate));
    let (issuer, serial) = issuer_and_serial(content(tbs));
    let sha256 = der(
        0x30,
        &[
            oid(&[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01]),
            der(0x05, &[]),
        ]
        .concat(),
    );
    let signer_info = der(
        0x30,
        &[
            der(0x02, &[1]),
            der(0x30, &[issuer, serial].concat()),
            sha256.clone(),
            der(
                0x30,
                &oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
            ),
            der(0x04, &key.sign(signature_file.as_bytes())),
        ]
        .concat(),
    );
    let signed_data = der(
        0x30,
        &[
            der(0x02, &[1]),
            der(0x31, &sha256),
            der(
                0x30,
                &oid(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01]),
            ),
            der(0xa0, certificate),
            der(0x31, &signer_info),
        ]
        .concat(),
    );
    let block = der(
        0x30,
        &[
            oid(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02]),
            der(0xa0, &signed_data),
        ]
        .concat(),
    );
    apk.file("META-INF/MANIFEST.MF", manifest.as_bytes())
        .file("META-INF/CERT.SF", signature_file.as_bytes())
        .file("META-INF/CERT.EC", &block)
}

/// Splits one DER element off `bytes`: its whole encoding and what follows.
fn element(bytes: &[u8]) -> (&[u8], &[u8]) {
    let (header, length) = match bytes[1] {
        length @ 0..=0x7f => (2, usize::from(length)),
        0x81 => (3, usize::from(bytes[2])),
        _ => (4, usize::from(bytes[2]) << 8 | usize::from(bytes[3])),
    };
    bytes.split_at(header + length)
}

fn content(element: &[u8]) -> &[u8] {
    let header = match element[1] {
        0..=0x7f => 2,
        0x81 => 3,
        _ => 4,
    };
    &element[header..]
}

fn issuer_and_serial(tbs: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (_version, rest) = element(tbs);
    let (serial, rest) = element(rest);
    let (_algorithm, rest) = element(rest);
    let (issuer, _) = element(rest);
    (issuer.to_vec(), serial.to_vec())
}
