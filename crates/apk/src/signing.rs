//! Verifies APK signatures as Android's installer does: the APK Signature Scheme v3 or v2 block when
//! the APK has one, otherwise the JAR signature (v1). Only the strongest scheme present is checked,
//! as Android does, and a weaker one that says a stronger one was stripped is refused.
//!
//! Everything is read from untrusted input: lengths are checked against what holds them, sizes are
//! capped, and signed lists are walked by their declared lengths, never past them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use rsa::pkcs8::DecodePublicKey;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use zip::ZipArchive;

use crate::Error;
use crate::der::{self, Reader};

/// Android 16, the framework uxdroid runs: v3 signers are chosen for it.
pub const PLATFORM_SDK: u32 = 36;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const EOCD_SIZE: usize = 22;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
const BLOCK_V2: u32 = 0x7109_871a;
const BLOCK_V3: u32 = 0xf053_68c0;
const BLOCK_V31: u32 = 0x1b93_ad61;
/// A v2 signer attribute saying the APK also had a v3 signature.
const STRIPPING_PROTECTION: u32 = 0xbeef_f00d;
/// Real blocks hold a few certificates; the limit stops a crafted size from costing memory.
const MAX_BLOCK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SIGNERS: usize = 10;
const CHUNK_BYTES: u64 = 1024 * 1024;
const MAX_JAR_METADATA_BYTES: u64 = 64 * 1024 * 1024;
/// v1 digests every entry; the cap stops decompression bombs.
const MAX_JAR_CONTENT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scheme {
    V1,
    V2,
    V3,
}

impl std::fmt::Display for Scheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Scheme::V1 => "v1",
            Scheme::V2 => "v2",
            Scheme::V3 => "v3",
        })
    }
}

/// The v3 signer attribute that proves a key rotation: the signing certificate lineage.
const PROOF_OF_ROTATION: u32 = 0x3ba0_6f8c;
/// Real lineages hold a handful of certificates; the limit stops a crafted one from costing time.
const MAX_LINEAGE: usize = 16;
/// The capability of a past signer whose apps' installed data the new key may keep, which is what
/// lets an app signed by the new key update one installed with the old.
pub const INSTALLED_DATA: u32 = 1;

/// A verified signature: the scheme checked and each signer's certificate, DER encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub scheme: Scheme,
    pub certificates: Vec<Vec<u8>>,
    /// For a v3 signer whose key rotated, the certificates it rotated from, oldest first, each
    /// with the capabilities the lineage gives it; checked to be a chain where each key signed the
    /// next, ending with the signer's own.
    pub rotated_from: Vec<(Vec<u8>, u32)>,
}

impl Signature {
    /// The SHA-256 of each signer's certificate in hex, sorted, as `apksigner` prints them.
    pub fn fingerprints(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .certificates
            .iter()
            .map(|certificate| hex(&Sha256::digest(certificate)))
            .collect();
        set.into_iter().collect()
    }
    /// The SHA-256 of each certificate the signer's key rotated from, oldest first, with its
    /// capabilities.
    pub fn past_fingerprints(&self) -> Vec<(String, u32)> {
        self.rotated_from
            .iter()
            .map(|(certificate, flags)| (hex(&Sha256::digest(certificate)), *flags))
            .collect()
    }

    /// Whether the certificate with SHA-256 `fingerprint` signs this, or signed an earlier key it
    /// rotated from with every capability in `needed`.
    pub fn descends_from(&self, fingerprint: &str, needed: u32) -> bool {
        self.fingerprints().iter().any(|own| own == fingerprint)
            || self
                .past_fingerprints()
                .iter()
                .any(|(past, flags)| past == fingerprint && flags & needed == needed)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn bad(reason: impl Into<String>) -> Error {
    Error::BadSignature(reason.into())
}

/// Verifies the signature of the APK at `path`.
pub fn verify_signature(path: &Path) -> Result<Signature, Error> {
    let unreadable = |source| Error::Unreadable(path.to_path_buf(), source);
    let mut file = File::open(path).map_err(unreadable)?;
    // Which of two entries with one name is read depends on the reader; Android refuses the whole
    // archive, and so does this, before anything else.
    let archive = ZipArchive::new(BufReader::new(&mut file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    if archive.has_duplicate_names() {
        return Err(Error::InvalidArchive(
            "it names an entry twice, which Android refuses".to_string(),
        ));
    }
    drop(archive);
    let sections = zip_sections(&mut file).map_err(|error| match error {
        Located::Io(source) => unreadable(source),
        Located::Bad(reason) => Error::InvalidArchive(reason.to_string()),
    })?;
    let block = match signing_block(&mut file, &sections) {
        Ok(block) => block,
        Err(Located::Io(source)) => return Err(unreadable(source)),
        Err(Located::Bad(reason)) => return Err(bad(reason)),
    };
    let Some(block) = block else {
        return verify_jar(path, &mut file);
    };
    let pairs = block_pairs(&block.bytes)?;
    let find = |id| pairs.iter().find(|(pair, _)| *pair == id).map(|(_, v)| *v);
    let v3 = find(BLOCK_V31)
        .and_then(|value| v3_signer(value).transpose())
        .or_else(|| find(BLOCK_V3).map(|value| v3_signer(value).and_then(required_v3)));
    if let Some(signer) = v3 {
        let signer = signer?;
        verify_signers(&mut file, &sections, &block, &[signer])?;
        let rotated_from = match signer.lineage {
            Some(lineage) => past_signers(lineage, signer.certificate)?,
            None => Vec::new(),
        };
        return Ok(Signature {
            scheme: Scheme::V3,
            certificates: vec![signer.certificate.to_vec()],
            rotated_from,
        });
    }
    if let Some(value) = find(BLOCK_V2) {
        let signers = v2_signers(value)?;
        if signers.iter().any(|signer| signer.claims_v3) {
            return Err(bad(
                "it says it was also signed with scheme v3, and that signature was removed",
            ));
        }
        verify_signers(&mut file, &sections, &block, &signers)?;
        return Ok(Signature {
            scheme: Scheme::V2,
            certificates: signers.iter().map(|s| s.certificate.to_vec()).collect(),
            rotated_from: Vec::new(),
        });
    }
    verify_jar(path, &mut file)
}

fn required_v3(signer: Option<Signer<'_>>) -> Result<Signer<'_>, Error> {
    signer.ok_or(bad(format!(
        "its v3 signature has no signer for SDK {PLATFORM_SDK}"
    )))
}

enum Located {
    Io(io::Error),
    Bad(&'static str),
}

impl From<io::Error> for Located {
    fn from(error: io::Error) -> Self {
        Located::Io(error)
    }
}

/// Where the archive's central directory and end record are.
struct Sections {
    central_directory: u64,
    end_record: u64,
    /// The end record and its comment.
    end: Vec<u8>,
}

fn zip_sections(file: &mut File) -> Result<Sections, Located> {
    let length = file.seek(SeekFrom::End(0))?;
    let tail_length = length.min(EOCD_SIZE as u64 + 0xffff);
    let mut tail = vec![0; tail_length as usize];
    file.seek(SeekFrom::Start(length - tail_length))?;
    file.read_exact(&mut tail)?;
    // The record is found from the end, the first whose comment length reaches the end exactly.
    let at = (0..=tail.len().saturating_sub(EOCD_SIZE))
        .rev()
        .find(|&at| {
            le32(&tail, at) == Some(EOCD_SIGNATURE)
                && le16(&tail, at + 20).map(usize::from) == Some(tail.len() - at - EOCD_SIZE)
        })
        .ok_or(Located::Bad("no end of central directory record"))?;
    let end_record = length - tail_length + at as u64;
    if at >= 20 && le32(&tail, at - 20) == Some(ZIP64_LOCATOR_SIGNATURE) {
        return Err(Located::Bad("ZIP64 archives cannot be verified"));
    }
    let size = u64::from(le32(&tail, at + 12).unwrap_or(u32::MAX));
    let central_directory = u64::from(le32(&tail, at + 16).unwrap_or(u32::MAX));
    if central_directory.checked_add(size) != Some(end_record) {
        return Err(Located::Bad(
            "the central directory does not end at its end record",
        ));
    }
    Ok(Sections {
        central_directory,
        end_record,
        end: tail[at..].to_vec(),
    })
}

struct Block {
    offset: u64,
    bytes: Vec<u8>,
}

/// The APK Signing Block, which sits right before the central directory, if there is one.
fn signing_block(file: &mut File, sections: &Sections) -> Result<Option<Block>, Located> {
    let end = sections.central_directory;
    if end < 32 {
        return Ok(None);
    }
    let mut footer = [0u8; 24];
    file.seek(SeekFrom::Start(end - 24))?;
    file.read_exact(&mut footer)?;
    if &footer[8..] != BLOCK_MAGIC {
        return Ok(None);
    }
    let size = u64::from_le_bytes(footer[..8].try_into().unwrap_or_default());
    let total = size
        .checked_add(8)
        .filter(|total| size >= 24 && *total <= end && *total <= MAX_BLOCK_BYTES)
        .ok_or(Located::Bad("the signing block has a bad size"))?;
    let offset = end - total;
    let mut bytes = vec![0; total as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    if bytes[..8] != footer[..8] {
        return Err(Located::Bad("the signing block's two sizes differ"));
    }
    Ok(Some(Block { offset, bytes }))
}

/// The ID-value pairs between the block's leading size and its footer.
fn block_pairs(block: &[u8]) -> Result<Vec<(u32, &[u8])>, Error> {
    let mut pairs = Vec::new();
    let mut rest = &block[8..block.len() - 24];
    while !rest.is_empty() {
        let length = rest
            .get(..8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap_or_default()))
            .ok_or(bad("truncated signing block pair"))?;
        let value = usize::try_from(length)
            .ok()
            .filter(|length| *length >= 4)
            .and_then(|length| rest.get(8..length.checked_add(8)?))
            .ok_or(bad("a signing block pair has a bad size"))?;
        let id = le32(value, 0).unwrap_or_default();
        pairs.push((id, &value[4..]));
        rest = &rest[8 + value.len()..];
    }
    Ok(pairs)
}

/// A list of `u32`-length-prefixed values.
struct Prefixed<'a>(&'a [u8]);

impl<'a> Prefixed<'a> {
    fn next(&mut self) -> Result<&'a [u8], Error> {
        let length = le32(self.0, 0).ok_or(bad("truncated signature data"))? as usize;
        let value = self
            .0
            .get(4..)
            .and_then(|rest| rest.get(..length))
            .ok_or(bad("a signature length reaches past its data"))?;
        self.0 = &self.0[4 + length..];
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let value = le32(self.0, 0).ok_or(bad("truncated signature data"))?;
        self.0 = &self.0[4..];
        Ok(value)
    }

    fn all(mut self) -> Result<Vec<&'a [u8]>, Error> {
        let mut values = Vec::new();
        while !self.0.is_empty() {
            values.push(self.next()?);
        }
        Ok(values)
    }
}

#[derive(Clone, Copy)]
struct Signer<'a> {
    signed_data: &'a [u8],
    /// The algorithm checked and its signature.
    algorithm: Algorithm,
    signature: &'a [u8],
    /// The content digest the signed data holds for that algorithm.
    digest: &'a [u8],
    public_key: &'a [u8],
    certificate: &'a [u8],
    claims_v3: bool,
    /// A v3 signer's proof of rotation, unchecked.
    lineage: Option<&'a [u8]>,
}

fn v2_signers(value: &[u8]) -> Result<Vec<Signer<'_>>, Error> {
    let signers = Prefixed(Prefixed(value).next()?).all()?;
    if signers.is_empty() || signers.len() > MAX_SIGNERS {
        return Err(bad("its v2 signature has no signers, or too many"));
    }
    signers
        .into_iter()
        .map(|signer| {
            let mut fields = Prefixed(signer);
            let signed_data = fields.next()?;
            let signatures = fields.next()?;
            let public_key = fields.next()?;
            let mut data = Prefixed(signed_data);
            let digests = data.next()?;
            let certificates = data.next()?;
            let attributes = Prefixed(data.next()?).all()?;
            let claims_v3 = attributes.iter().any(|attribute| {
                le32(attribute, 0) == Some(STRIPPING_PROTECTION) && le32(attribute, 4) == Some(3)
            });
            let mut signer =
                signer_from(signed_data, digests, certificates, signatures, public_key)?;
            signer.claims_v3 = claims_v3;
            Ok(signer)
        })
        .collect()
}

/// The v3 signer for this platform's SDK, if any.
fn v3_signer(value: &[u8]) -> Result<Option<Signer<'_>>, Error> {
    let signers = Prefixed(Prefixed(value).next()?).all()?;
    if signers.len() > MAX_SIGNERS {
        return Err(bad("its v3 signature has too many signers"));
    }
    for signer in signers {
        let mut fields = Prefixed(signer);
        let signed_data = fields.next()?;
        let (min, max) = (fields.u32()?, fields.u32()?);
        let signatures = fields.next()?;
        let public_key = fields.next()?;
        let mut data = Prefixed(signed_data);
        let digests = data.next()?;
        let certificates = data.next()?;
        if (data.u32()?, data.u32()?) != (min, max) {
            return Err(bad("a v3 signer's SDK range differs from its signed one"));
        }
        if (min..=max).contains(&PLATFORM_SDK) {
            // Older signers have no attributes after the SDK range.
            let attributes = if data.0.is_empty() {
                Vec::new()
            } else {
                Prefixed(data.next()?).all()?
            };
            let mut lineage = None;
            for attribute in attributes {
                if le32(attribute, 0) == Some(PROOF_OF_ROTATION) {
                    if lineage.is_some() {
                        return Err(bad("its v3 signer proves a key rotation twice"));
                    }
                    lineage = Some(&attribute[4..]);
                }
            }
            let mut signer =
                signer_from(signed_data, digests, certificates, signatures, public_key)?;
            signer.lineage = lineage;
            return Ok(Some(signer));
        }
    }
    Ok(None)
}

fn signer_from<'a>(
    signed_data: &'a [u8],
    digests: &'a [u8],
    certificates: &'a [u8],
    signatures: &'a [u8],
    public_key: &'a [u8],
) -> Result<Signer<'a>, Error> {
    let pair = |item: &'a [u8]| -> Result<(u32, &'a [u8]), Error> {
        let mut fields = Prefixed(item);
        Ok((fields.u32()?, fields.next()?))
    };
    let signatures: Vec<(u32, &[u8])> = Prefixed(signatures)
        .all()?
        .into_iter()
        .map(pair)
        .collect::<Result<_, _>>()?;
    let digests: Vec<(u32, &[u8])> = Prefixed(digests)
        .all()?
        .into_iter()
        .map(pair)
        .collect::<Result<_, _>>()?;
    let ids = |list: &[(u32, &[u8])]| list.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    if ids(&signatures) != ids(&digests) {
        return Err(bad("its signatures and digests name different algorithms"));
    }
    let (algorithm, signature) = signatures
        .iter()
        .filter_map(|(id, signature)| Algorithm::from_id(*id).map(|alg| (alg, *signature)))
        .max_by_key(|(algorithm, _)| algorithm.rank())
        .ok_or_else(|| {
            bad(format!(
                "it uses no signature algorithm this verifier supports ({})",
                ids(&signatures)
                    .iter()
                    .map(|id| format!("{id:#06x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let digest = digests
        .iter()
        .find(|(id, _)| Algorithm::from_id(*id) == Some(algorithm))
        .map(|(_, digest)| *digest)
        .unwrap_or_default();
    let certificate = *Prefixed(certificates)
        .all()?
        .first()
        .ok_or(bad("a signer has no certificate"))?;
    Ok(Signer {
        signed_data,
        algorithm,
        signature,
        digest,
        public_key,
        certificate,
        claims_v3: false,
        lineage: None,
    })
}

/// Checks a proof of rotation, as Android does, and returns the certificates before `signer`'s:
/// version 1, then one node per certificate from the oldest, each node's certificate and the
/// algorithm that signs it signed by the key before it, with that key's capabilities and the
/// algorithm it signs the next node with; the last certificate must be the signer's own.
fn past_signers(lineage: &[u8], signer: &[u8]) -> Result<Vec<(Vec<u8>, u32)>, Error> {
    let refused = |why: &str| bad(format!("its key rotation proof {why}"));
    let mut fields = Prefixed(lineage);
    if fields.u32()? != 1 {
        return Err(refused("has a version this verifier does not know"));
    }
    let nodes = fields.all()?;
    if nodes.is_empty() || nodes.len() > MAX_LINEAGE {
        return Err(refused("has no certificates, or too many"));
    }
    let mut chain: Vec<(&[u8], u32)> = Vec::new();
    let mut previous: Option<(&[u8], u32)> = None;
    for node in nodes {
        let mut node = Prefixed(node);
        let signed = node.next()?;
        let (flags, next_algorithm) = (node.u32()?, node.u32()?);
        let signature = node.next()?;
        let mut contents = Prefixed(signed);
        let certificate = contents.next()?;
        let signed_with = contents.u32()?;
        if let Some((key, algorithm)) = previous {
            if signed_with != algorithm {
                return Err(refused(
                    "names another algorithm than the one that signs it",
                ));
            }
            let algorithm = Algorithm::from_id(algorithm).ok_or_else(|| {
                refused("uses a signature algorithm this verifier does not support")
            })?;
            let key = der::certificate(key)
                .map_err(|reason| refused(&format!("has a malformed certificate: {reason}")))?;
            algorithm
                .verify(key.public_key, signed, signature)
                .map_err(|_| refused("is not signed by the key it rotated from"))?;
        }
        if chain.iter().any(|(seen, _)| *seen == certificate) {
            return Err(refused("names a certificate twice"));
        }
        chain.push((certificate, flags));
        previous = Some((certificate, next_algorithm));
    }
    let (last, _) = chain.pop().expect("one node at least");
    if last != signer {
        return Err(refused("does not end with the certificate that signed it"));
    }
    Ok(chain
        .into_iter()
        .map(|(certificate, flags)| (certificate.to_vec(), flags))
        .collect())
}

fn verify_signers(
    file: &mut File,
    sections: &Sections,
    block: &Block,
    signers: &[Signer<'_>],
) -> Result<(), Error> {
    for signer in signers {
        let certificate = der::certificate(signer.certificate)
            .map_err(|reason| bad(format!("a signer's certificate is malformed: {reason}")))?;
        if certificate.public_key != signer.public_key {
            return Err(bad("a signer's public key differs from its certificate's"));
        }
        signer
            .algorithm
            .verify(signer.public_key, signer.signed_data, signer.signature)?;
    }
    let hashes: BTreeSet<Hash> = signers.iter().map(|s| s.algorithm.hash).collect();
    let digests = content_digests(file, sections, block.offset, &hashes)
        .map_err(|error| bad(format!("cannot read it to check its digest: {error}")))?;
    for signer in signers {
        if digests.get(&signer.algorithm.hash).map(Vec::as_slice) != Some(signer.digest) {
            return Err(bad(
                "its contents do not match their signed digest, so it was changed after signing",
            ));
        }
    }
    Ok(())
}

/// The chunked digests v2 and v3 sign: the entries, the central directory, and the end record
/// pointing at where the signing block starts, in 1 MiB chunks.
fn content_digests(
    file: &mut File,
    sections: &Sections,
    block_offset: u64,
    hashes: &BTreeSet<Hash>,
) -> io::Result<BTreeMap<Hash, Vec<u8>>> {
    let mut end = sections.end.clone();
    end[16..20].copy_from_slice(&(block_offset as u32).to_le_bytes());
    let ranges = [
        (0, block_offset),
        (
            sections.central_directory,
            sections.end_record - sections.central_directory,
        ),
    ];
    let chunks = ranges
        .iter()
        .map(|(_, length)| length.div_ceil(CHUNK_BYTES))
        .sum::<u64>()
        + (end.len() as u64).div_ceil(CHUNK_BYTES);
    let mut tops: BTreeMap<Hash, Vec<u8>> = hashes
        .iter()
        .map(|hash| {
            let mut top = vec![0x5a];
            top.extend_from_slice(&(chunks as u32).to_le_bytes());
            (*hash, top)
        })
        .collect();
    let mut add_chunk = |chunk: &[u8]| {
        for (hash, top) in tops.iter_mut() {
            let mut prefix = vec![0xa5];
            prefix.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            top.extend(hash.digest_parts(&[&prefix, chunk]));
        }
    };
    let mut buffer = vec![0; CHUNK_BYTES as usize];
    let mut reader = BufReader::new(&mut *file);
    for (start, length) in ranges {
        reader.seek(SeekFrom::Start(start))?;
        let mut left = length;
        while left > 0 {
            let size = left.min(CHUNK_BYTES) as usize;
            reader.read_exact(&mut buffer[..size])?;
            add_chunk(&buffer[..size]);
            left -= size as u64;
        }
    }
    end.chunks(CHUNK_BYTES as usize).for_each(&mut add_chunk);
    Ok(tops
        .into_iter()
        .map(|(hash, top)| (hash, hash.digest_parts(&[&top])))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn digest_parts(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut hasher = D::new();
            parts.iter().for_each(|part| hasher.update(part));
            hasher.finalize().to_vec()
        }
        match self {
            Hash::Sha1 => run::<Sha1>(parts),
            Hash::Sha256 => run::<Sha256>(parts),
            Hash::Sha384 => run::<Sha384>(parts),
            Hash::Sha512 => run::<Sha512>(parts),
        }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        self.digest_parts(&[data])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Padding {
    Pkcs1,
    Pss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Algorithm {
    hash: Hash,
    /// For RSA keys; EC keys ignore it.
    padding: Padding,
}

impl Algorithm {
    /// The signature algorithm IDs of APK Signature Scheme v2 and v3. DSA and the verity variants,
    /// which sign a Merkle tree root, are not supported.
    fn from_id(id: u32) -> Option<Self> {
        let (hash, padding) = match id {
            0x0101 => (Hash::Sha256, Padding::Pss),
            0x0102 => (Hash::Sha512, Padding::Pss),
            0x0103 | 0x0201 => (Hash::Sha256, Padding::Pkcs1),
            0x0104 | 0x0202 => (Hash::Sha512, Padding::Pkcs1),
            _ => return None,
        };
        Some(Self { hash, padding })
    }

    fn rank(self) -> (Hash, bool) {
        (self.hash, self.padding == Padding::Pss)
    }

    fn verify(self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), Error> {
        verify_with_key(public_key, self.hash, self.padding, message, signature)
    }
}

fn verify_with_key(
    public_key: &[u8],
    hash: Hash,
    padding: Padding,
    message: &[u8],
    signature: &[u8],
) -> Result<(), Error> {
    use p256::ecdsa::signature::hazmat::PrehashVerifier;

    let digest = hash.digest(message);
    let failed = || bad("a signature does not verify");
    if let Ok(key) = rsa::RsaPublicKey::from_public_key_der(public_key) {
        let scheme_result = match (padding, hash) {
            (Padding::Pkcs1, Hash::Sha1) => {
                key.verify(rsa::Pkcs1v15Sign::new::<Sha1>(), &digest, signature)
            }
            (Padding::Pkcs1, Hash::Sha256) => {
                key.verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &digest, signature)
            }
            (Padding::Pkcs1, Hash::Sha384) => {
                key.verify(rsa::Pkcs1v15Sign::new::<Sha384>(), &digest, signature)
            }
            (Padding::Pkcs1, Hash::Sha512) => {
                key.verify(rsa::Pkcs1v15Sign::new::<Sha512>(), &digest, signature)
            }
            (Padding::Pss, Hash::Sha256) => {
                key.verify(rsa::Pss::new::<Sha256>(), &digest, signature)
            }
            (Padding::Pss, Hash::Sha512) => {
                key.verify(rsa::Pss::new::<Sha512>(), &digest, signature)
            }
            (Padding::Pss, _) => return Err(failed()),
        };
        return scheme_result.map_err(|_| failed());
    }
    if let Ok(key) = p256::PublicKey::from_public_key_der(public_key) {
        let signature = p256::ecdsa::Signature::from_der(signature).map_err(|_| failed())?;
        return p256::ecdsa::VerifyingKey::from(key)
            .verify_prehash(&digest, &signature)
            .map_err(|_| failed());
    }
    if let Ok(key) = p384::PublicKey::from_public_key_der(public_key) {
        let signature = p384::ecdsa::Signature::from_der(signature).map_err(|_| failed())?;
        return p384::ecdsa::VerifyingKey::from(key)
            .verify_prehash(&digest, &signature)
            .map_err(|_| failed());
    }
    Err(bad(
        "a signer's key is not one this verifier supports (RSA up to 4096 bits, EC P-256 or P-384)",
    ))
}

// JAR signing (v1).

/// One section of a JAR manifest or signature file: its bytes and its attributes.
struct Section<'a> {
    bytes: &'a [u8],
    attributes: Vec<(String, String)>,
}

impl Section<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    /// The strongest digest this section has under `<algorithm><suffix>`.
    fn digest(&self, suffix: &str) -> Option<(Hash, Vec<u8>)> {
        [
            (Hash::Sha512, "SHA-512"),
            (Hash::Sha384, "SHA-384"),
            (Hash::Sha256, "SHA-256"),
            (Hash::Sha1, "SHA1"),
            (Hash::Sha1, "SHA-1"),
        ]
        .into_iter()
        .find_map(|(hash, name)| {
            let value = self.get(&format!("{name}{suffix}"))?;
            Some((hash, base64(value)?))
        })
    }
}

/// Splits a manifest into its sections. Each section's bytes run through the blank line that ends
/// it, which is what signature files digest.
fn sections(text: &[u8]) -> Result<Vec<Section<'_>>, Error> {
    let malformed = || bad("a JAR manifest is malformed");
    let mut sections = Vec::new();
    let mut start = 0;
    let mut position = 0;
    let mut attributes: Vec<(String, String)> = Vec::new();
    while position < text.len() {
        let line_end = text[position..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |at| position + at + 1);
        let line = &text[position..line_end];
        let content = line
            .strip_suffix(b"\n")
            .unwrap_or(line)
            .strip_suffix(b"\r")
            .unwrap_or(line.strip_suffix(b"\n").unwrap_or(line));
        position = line_end;
        if content.is_empty() {
            sections.push(Section {
                bytes: &text[start..position],
                attributes: std::mem::take(&mut attributes),
            });
            start = position;
        } else if let Some(more) = content.strip_prefix(b" ") {
            let (_, value) = attributes.last_mut().ok_or_else(malformed)?;
            value.push_str(std::str::from_utf8(more).map_err(|_| malformed())?);
        } else {
            let content = std::str::from_utf8(content).map_err(|_| malformed())?;
            let (name, value) = content.split_once(": ").ok_or_else(malformed)?;
            attributes.push((name.to_string(), value.to_string()));
        }
    }
    if start < text.len() {
        sections.push(Section {
            bytes: &text[start..],
            attributes,
        });
    }
    Ok(sections)
}

/// Verifies the JAR signature (v1) of a signed JAR held in memory, such as an F-Droid repository's
/// `entry.jar`, with the rules APKs get: every entry outside `META-INF/` must be signed by every
/// signer, and the signature file must not claim a stronger scheme. Returns each signer's
/// certificate; which of them to trust is the caller's choice.
pub fn verify_jar_signature(jar: &[u8]) -> Result<Signature, Error> {
    verify_jar(Path::new("<memory>"), &mut io::Cursor::new(jar))
}

fn verify_jar<R: Read + Seek>(path: &Path, file: &mut R) -> Result<Signature, Error> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(&mut *file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    if archive.has_duplicate_names() {
        return Err(Error::InvalidArchive(
            "it names an entry twice, which Android refuses".to_string(),
        ));
    }
    let names: Vec<String> = archive
        .file_names()
        .map(|name| name.map(|name| name.to_string()))
        .collect::<Result<_, _>>()
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let blocks: Vec<&String> = names
        .iter()
        .filter(|name| {
            name.strip_prefix("META-INF/").is_some_and(|file| {
                !file.contains('/')
                    && [".RSA", ".DSA", ".EC"]
                        .iter()
                        .any(|extension| file.ends_with(extension))
            })
        })
        .collect();
    if blocks.is_empty() {
        return Err(Error::Unsigned);
    }
    let read = |archive: &mut ZipArchive<_>, name: &str| -> Result<Vec<u8>, Error> {
        let entry = archive
            .by_name(name)
            .map_err(|_| bad(format!("it has no {}", name.escape_debug())))?;
        let mut bytes = Vec::new();
        entry
            .take(MAX_JAR_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| bad(format!("cannot read {}: {error}", name.escape_debug())))?;
        if bytes.len() as u64 > MAX_JAR_METADATA_BYTES {
            return Err(bad(format!("{} is too large", name.escape_debug())));
        }
        Ok(bytes)
    };
    let manifest = read(&mut archive, "META-INF/MANIFEST.MF")?;
    let manifest_sections = sections(&manifest)?;
    let mut entries: BTreeMap<&str, &Section> = BTreeMap::new();
    for section in manifest_sections.iter().skip(1) {
        if let Some(name) = section.get("Name")
            && entries.insert(name, section).is_some()
        {
            return Err(bad(format!(
                "its JAR manifest names {} twice",
                name.escape_debug()
            )));
        }
    }

    let mut certificates = Vec::new();
    let mut signed_by_all: Option<BTreeSet<String>> = None;
    for block_name in blocks {
        let stem = &block_name[..block_name.rfind('.').unwrap_or(block_name.len())];
        let signature_file = read(&mut archive, &format!("{stem}.SF"))?;
        let block = read(&mut archive, block_name)?;
        certificates.push(verify_pkcs7(&block, &signature_file)?);
        let file_sections = sections(&signature_file)?;
        let main = file_sections
            .first()
            .ok_or(bad("a signature file is empty"))?;
        if let Some(schemes) = main.get("X-Android-APK-Signed")
            && schemes
                .split(',')
                .any(|scheme| matches!(scheme.trim(), "2" | "3"))
        {
            return Err(bad(
                "it says it was also signed with scheme v2 or v3, and that signature was removed",
            ));
        }
        let mut signed = BTreeSet::new();
        let whole = main.digest("-Digest-Manifest");
        if whole
            .as_ref()
            .is_some_and(|(hash, digest)| hash.digest(&manifest) == *digest)
        {
            signed.extend(entries.keys().map(|name| name.to_string()));
        } else {
            for section in file_sections.iter().skip(1) {
                let Some(name) = section.get("Name") else {
                    continue;
                };
                let entry = entries.get(name).ok_or(bad(format!(
                    "its signature file signs {}, which its manifest does not list",
                    name.escape_debug()
                )))?;
                let (hash, digest) = section.digest("-Digest").ok_or(bad(format!(
                    "its signature file has no digest for {}",
                    name.escape_debug()
                )))?;
                if hash.digest(entry.bytes) != digest {
                    return Err(bad(format!(
                        "its manifest section for {} does not match its signed digest",
                        name.escape_debug()
                    )));
                }
                signed.insert(name.to_string());
            }
        }
        signed_by_all = Some(match signed_by_all {
            None => signed,
            Some(all) => all.intersection(&signed).cloned().collect(),
        });
    }
    let signed_by_all = signed_by_all.unwrap_or_default();

    let mut total = 0u64;
    let mut buffer = vec![0; 64 * 1024];
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|source| Error::InvalidArchive(source.to_string()))?;
        let name = entry
            .name()
            .map_err(|source| Error::InvalidArchive(source.to_string()))?
            .to_string();
        if entry.is_dir() || name.starts_with("META-INF/") {
            continue;
        }
        let section = entries
            .get(name.as_str())
            .filter(|_| signed_by_all.contains(&name))
            .ok_or(bad(format!("{} is not signed", name.escape_debug())))?;
        let (hash, expected) = section.digest("-Digest").ok_or(bad(format!(
            "its manifest has no digest for {}",
            name.escape_debug()
        )))?;
        let mut digest = DigestState::new(hash);
        loop {
            let read = entry
                .read(&mut buffer)
                .map_err(|error| bad(format!("cannot read {}: {error}", name.escape_debug())))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > MAX_JAR_CONTENT_BYTES {
                return Err(bad("its entries are too large to verify"));
            }
            digest.update(&buffer[..read]);
        }
        if digest.finish() != expected {
            return Err(bad(format!(
                "{} does not match its signed digest, so it was changed after signing",
                name.escape_debug()
            )));
        }
    }
    Ok(Signature {
        scheme: Scheme::V1,
        certificates,
        rotated_from: Vec::new(),
    })
}

enum DigestState {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl DigestState {
    fn new(hash: Hash) -> Self {
        match hash {
            Hash::Sha1 => DigestState::Sha1(Sha1::new()),
            Hash::Sha256 => DigestState::Sha256(Sha256::new()),
            Hash::Sha384 => DigestState::Sha384(Sha384::new()),
            Hash::Sha512 => DigestState::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            DigestState::Sha1(state) => state.update(data),
            DigestState::Sha256(state) => state.update(data),
            DigestState::Sha384(state) => state.update(data),
            DigestState::Sha512(state) => state.update(data),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            DigestState::Sha1(state) => state.finalize().to_vec(),
            DigestState::Sha256(state) => state.finalize().to_vec(),
            DigestState::Sha384(state) => state.finalize().to_vec(),
            DigestState::Sha512(state) => state.finalize().to_vec(),
        }
    }
}

const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";

fn hash_of(oid: &[u8]) -> Option<Hash> {
    [
        ("1.3.14.3.2.26", Hash::Sha1),
        ("2.16.840.1.101.3.4.2.1", Hash::Sha256),
        ("2.16.840.1.101.3.4.2.2", Hash::Sha384),
        ("2.16.840.1.101.3.4.2.3", Hash::Sha512),
    ]
    .into_iter()
    .find(|(text, _)| der::oid(text) == oid)
    .map(|(_, hash)| hash)
}

/// Verifies a PKCS #7 signature block over a signature file and returns the signer's certificate.
fn verify_pkcs7(block: &[u8], signed: &[u8]) -> Result<Vec<u8>, Error> {
    let malformed = |reason: &str| bad(format!("a JAR signature block is malformed: {reason}"));
    let parse = || -> Result<_, &'static str> {
        let content_info = Reader::new(block).expect(der::SEQUENCE)?;
        let mut content_info = Reader::new(content_info.content);
        if content_info.expect(der::OID)?.content != der::oid(OID_SIGNED_DATA) {
            return Err("not signed data");
        }
        let explicit = content_info.expect(0xa0)?;
        let signed_data = Reader::new(explicit.content).expect(der::SEQUENCE)?;
        let mut fields = Reader::new(signed_data.content);
        fields.expect(der::INTEGER)?;
        fields.expect(der::SET)?;
        fields.expect(der::SEQUENCE)?;
        let certificates = fields.optional(0xa0)?;
        fields.optional(0xa1)?;
        let signer_infos = fields.expect(der::SET)?;
        let signer_info = Reader::new(signer_infos.content).expect(der::SEQUENCE)?;
        let mut info = Reader::new(signer_info.content);
        info.expect(der::INTEGER)?;
        let issuer_and_serial = info.expect(der::SEQUENCE)?;
        let digest_algorithm = info.expect(der::SEQUENCE)?;
        let attributes = info.optional(0xa0)?;
        info.expect(der::SEQUENCE)?;
        let signature = info.expect(der::OCTET_STRING)?.content;
        let mut issuer_and_serial = Reader::new(issuer_and_serial.content);
        let issuer = issuer_and_serial.expect(der::SEQUENCE)?.whole;
        let serial = issuer_and_serial.expect(der::INTEGER)?.content;
        let digest_oid = Reader::new(digest_algorithm.content)
            .expect(der::OID)?
            .content;
        Ok((
            certificates,
            issuer,
            serial,
            digest_oid,
            attributes,
            signature,
        ))
    };
    let (certificates, issuer, serial, digest_oid, attributes, signature) =
        parse().map_err(malformed)?;
    let hash = hash_of(digest_oid).ok_or(bad("a JAR signature uses an unsupported digest"))?;
    let mut list = Reader::new(certificates.map(|tlv| tlv.content).unwrap_or_default());
    let certificate = loop {
        if list.is_empty() {
            return Err(bad("a JAR signature block lacks its signer's certificate"));
        }
        let candidate = list.expect(der::SEQUENCE).map_err(malformed)?.whole;
        let parsed = der::certificate(candidate).map_err(malformed)?;
        if parsed.issuer == issuer && parsed.serial == serial {
            break (candidate, parsed.public_key);
        }
    };
    let message: Vec<u8> = match attributes {
        None => signed.to_vec(),
        Some(attributes) => {
            let mut reader = Reader::new(attributes.content);
            let mut digest = None;
            while !reader.is_empty() {
                let attribute = reader.expect(der::SEQUENCE).map_err(malformed)?;
                let mut fields = Reader::new(attribute.content);
                let oid = fields.expect(der::OID).map_err(malformed)?;
                if oid.content == der::oid(OID_MESSAGE_DIGEST) {
                    let values = fields.expect(der::SET).map_err(malformed)?;
                    digest = Some(
                        Reader::new(values.content)
                            .expect(der::OCTET_STRING)
                            .map_err(malformed)?
                            .content,
                    );
                }
            }
            if digest != Some(hash.digest(signed).as_slice()) {
                return Err(bad("a JAR signature file does not match its signed digest"));
            }
            // The signature covers the attributes encoded as a SET, not with their [0] tag.
            let mut encoded = attributes.whole.to_vec();
            encoded[0] = der::SET;
            encoded
        }
    };
    verify_with_key(certificate.1, hash, Padding::Pkcs1, &message, signature)?;
    Ok(certificate.0.to_vec())
}

/// Decodes standard base64 with padding, refusing anything else.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.trim().as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, quad) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let padding = quad.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut word = 0u32;
        for byte in &quad[..4 - padding] {
            word = (word << 6) | u32::from(value(*byte)?);
        }
        word <<= 6 * padding as u32;
        let decoded = word.to_be_bytes();
        out.extend_from_slice(&decoded[1..4 - padding]);
    }
    Some(out)
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_padding_and_refuses_bad_text() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGk=").unwrap(), b"hi");
        assert_eq!(base64("YWJj").unwrap(), b"abc");
        for bad in ["aGk", "a=Gk", "aGk*", "====", "aG=k"] {
            assert!(base64(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn manifest_sections_keep_their_bytes_and_continuations() {
        let text = b"Manifest-Version: 1.0\r\n\r\nName: a/very/long\r\n /name\r\nSHA-256-Digest: YWJj\r\n\r\n";
        let parsed = sections(text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].get("Name"), Some("a/very/long/name"));
        assert_eq!(
            parsed[1].bytes,
            b"Name: a/very/long\r\n /name\r\nSHA-256-Digest: YWJj\r\n\r\n"
        );
        assert!(sections(b" continuation first\n").is_err());
    }
}
