//! Apps delivered as a base APK plus split APKs: reading them out of a bundle (`.apks` from
//! bundletool, `.xapk`), checking that they belong together, and choosing the configuration splits
//! that fit this machine, as Play's installer does for a device.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

use crate::signing::{PLATFORM_SDK, Signature};
use crate::{Apk, Error, MANIFEST_ENTRY};

const TABLE_OF_CONTENTS: &str = "toc.pb";
const MAX_BUNDLE_APKS: usize = 1000;
/// Larger split APKs do not occur; the limits stop decompression bombs.
const MAX_BUNDLED_APK_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_TABLE_BYTES: u64 = 16 * 1024 * 1024;

/// The densities configuration splits are named after, in dots per inch.
const DENSITIES: [(&str, u32); 7] = [
    ("ldpi", 120),
    ("mdpi", 160),
    ("tvdpi", 213),
    ("hdpi", 240),
    ("xhdpi", 320),
    ("xxhdpi", 480),
    ("xxxhdpi", 640),
];
const ABIS: [&str; 8] = [
    "armeabi",
    "armeabi-v7a",
    "arm64-v8a",
    "x86",
    "x86_64",
    "mips",
    "mips64",
    "riscv64",
];

fn bad(reason: String) -> Error {
    Error::Splits(reason)
}

/// The APKs inside a bundle, if `path` is one: a ZIP archive of APKs rather than an APK. Nothing is
/// decompressed. With bundletool's table of contents, only the variant for this platform's SDK is
/// listed, since a bundle can hold one set of splits per SDK range.
pub fn bundle_entries(path: &Path) -> Result<Option<Vec<String>>, Error> {
    let file = File::open(path).map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    if archive.index_for_name(MANIFEST_ENTRY).is_some() {
        return Ok(None);
    }
    if archive.has_duplicate_names() {
        return Err(Error::InvalidArchive(
            "it names an entry twice, which Android refuses".to_string(),
        ));
    }
    let names: Vec<String> = archive
        .file_names()
        .map(|name| name.map(|name| name.to_string()))
        .filter(|name| name.as_ref().map_or(true, |name| name.ends_with(".apk")))
        .collect::<Result<_, _>>()
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let entries = if archive.index_for_name(TABLE_OF_CONTENTS).is_some() {
        let mut table = Vec::new();
        archive
            .by_name(TABLE_OF_CONTENTS)
            .map_err(|source| Error::InvalidArchive(source.to_string()))?
            .take(MAX_TABLE_BYTES + 1)
            .read_to_end(&mut table)
            .map_err(|source| Error::InvalidArchive(source.to_string()))?;
        let paths = variant_paths(&table)
            .map_err(|reason| bad(format!("its table of contents is malformed: {reason}")))?;
        if let Some(missing) = paths.iter().find(|path| !names.contains(path)) {
            return Err(bad(format!(
                "its table of contents names {}, which it does not hold",
                missing.escape_debug()
            )));
        }
        paths
    } else {
        // An archive with split APKs and standalone ones, for older systems, installs the splits.
        let (standalone, split): (Vec<String>, Vec<String>) = names
            .into_iter()
            .partition(|name| name.starts_with("standalones/"));
        if split.is_empty() { standalone } else { split }
    };
    if entries.is_empty() {
        return Err(Error::MissingManifest);
    }
    if entries.len() > MAX_BUNDLE_APKS {
        return Err(bad("it holds too many APKs".to_string()));
    }
    Ok(Some(entries))
}

/// Copies one APK out of a bundle to `destination`.
pub fn extract_bundled(path: &Path, entry: &str, destination: &Path) -> Result<(), Error> {
    let file = File::open(path).map_err(|source| Error::Unreadable(path.to_path_buf(), source))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let cannot =
        |error: io::Error| bad(format!("cannot extract {}: {error}", entry.escape_debug()));
    let source = archive
        .by_name(entry)
        .map_err(|source| Error::InvalidArchive(source.to_string()))?;
    let mut output = File::create(destination).map_err(cannot)?;
    let written =
        io::copy(&mut source.take(MAX_BUNDLED_APK_BYTES + 1), &mut output).map_err(cannot)?;
    if written > MAX_BUNDLED_APK_BYTES {
        return Err(bad(format!("{} is too large", entry.escape_debug())));
    }
    Ok(())
}

/// A protocol buffer field: its number and its value, a number or bytes.
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

fn fields(mut bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, &'static str> {
    fn varint(bytes: &mut &[u8]) -> Result<u64, &'static str> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = bytes.split_first().ok_or("truncated number")?;
            *bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("number too long")
    }
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        let value = match key & 7 {
            0 => Field::Number(varint(&mut bytes)?),
            1 | 5 => {
                let size = if key & 7 == 1 { 8 } else { 4 };
                let (_, rest) = bytes.split_at_checked(size).ok_or("truncated field")?;
                bytes = rest;
                Field::Number(0)
            }
            2 => {
                let length = usize::try_from(varint(&mut bytes)?).map_err(|_| "bad length")?;
                let (value, rest) = bytes.split_at_checked(length).ok_or("truncated field")?;
                bytes = rest;
                Field::Bytes(value)
            }
            _ => return Err("unsupported field type"),
        };
        out.push((key >> 3, value));
    }
    Ok(out)
}

fn messages<'a>(list: &[(u64, Field<'a>)], number: u64) -> impl Iterator<Item = &'a [u8]> {
    list.iter().filter_map(move |(field, value)| match value {
        Field::Bytes(bytes) if *field == number => Some(*bytes),
        _ => None,
    })
}

/// The APK paths of the variant bundletool's `BuildApksResult` targets at this platform: the split
/// variant with the highest minimum SDK not above it.
fn variant_paths(table: &[u8]) -> Result<Vec<String>, &'static str> {
    let mut best: Option<((bool, u64), Vec<String>)> = None;
    for variant in messages(&fields(table)?, 1) {
        let variant = fields(variant)?;
        // targeting.sdk_version_targeting.value[0].min.value
        let mut min_sdk = 0;
        if let Some(sdk) = messages(&variant, 1)
            .next()
            .map(fields)
            .transpose()?
            .and_then(|targeting| messages(&targeting, 1).next())
            .map(fields)
            .transpose()?
            .and_then(|sdk| messages(&sdk, 1).next())
            .map(fields)
            .transpose()?
            .and_then(|value| messages(&value, 1).next())
            .map(fields)
            .transpose()?
        {
            for (field, value) in sdk {
                if let (1, Field::Number(number)) = (field, value) {
                    min_sdk = number;
                }
            }
        }
        if min_sdk > u64::from(PLATFORM_SDK) {
            continue;
        }
        let mut paths = Vec::new();
        let mut splits = true;
        for set in messages(&variant, 2) {
            for description in messages(&fields(set)?, 2) {
                let description = fields(description)?;
                splits &= !description.iter().any(|(field, _)| *field == 4);
                let path = messages(&description, 2)
                    .next()
                    .ok_or("an APK has no path")?;
                paths.push(String::from_utf8(path.to_vec()).map_err(|_| "a path is not UTF-8")?);
            }
        }
        let rank = (splits, min_sdk);
        if best.as_ref().is_none_or(|(best, _)| rank > *best) {
            best = Some((rank, paths));
        }
    }
    best.map(|(_, paths)| paths)
        .ok_or("no variant runs on this SDK")
}

/// One APK of an app, read and verified.
pub struct Part {
    /// How messages name it: its path, or the bundle's and its entry's.
    pub origin: String,
    pub path: PathBuf,
    pub apk: Apk,
    pub signature: Signature,
}

/// What a split configures, from its name, as bundletool names them (`config.<value>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Abi(String),
    Density(u32),
    Language(String),
    Feature,
    /// A split whose purpose the name does not tell, which is always installed.
    Other,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Abi(_) => "abi",
            Kind::Density(_) => "density",
            Kind::Language(_) => "language",
            Kind::Feature => "feature",
            Kind::Other => "other",
        })
    }
}

/// What the configuration splits are chosen for.
pub struct Host {
    pub abi: String,
    /// The ABI this host runs through a translator, chosen when no split has code for `abi`.
    pub translated: Option<String>,
    pub density: u32,
    /// Language codes, such as `en`, in order of preference.
    pub languages: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// The index of the base APK among the parts.
    pub base: usize,
    /// The splits to install, by index, with what they configure, sorted by name.
    pub chosen: Vec<(usize, Kind)>,
    /// The splits left out because they fit another machine, by name.
    pub skipped: Vec<(String, Kind)>,
}

/// Split names become file names, so only the characters build tools use are accepted.
fn valid_split_name(name: &str) -> bool {
    !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn kind_of(name: &str, feature: bool) -> Kind {
    if feature {
        return Kind::Feature;
    }
    let Some(value) = name.rsplit_once("config.").map(|(_, value)| value) else {
        return Kind::Other;
    };
    if let Some(abi) = ABIS.iter().find(|abi| abi.replace('-', "_") == value) {
        return Kind::Abi(abi.to_string());
    }
    if let Some((_, dpi)) = DENSITIES.iter().find(|(name, _)| *name == value) {
        return Kind::Density(*dpi);
    }
    let language = value.split(['_', '-']).next().unwrap_or_default();
    if (2..=3).contains(&language.len()) && language.bytes().all(|b| b.is_ascii_lowercase()) {
        return Kind::Language(language.to_string());
    }
    Kind::Other
}

/// Checks that `parts` are one app's base and splits and chooses the splits this host installs.
pub fn plan_splits(parts: &[Part], host: &Host) -> Result<Plan, Error> {
    let bases: Vec<usize> = (0..parts.len())
        .filter(|index| parts[*index].apk.manifest.split.is_none())
        .collect();
    let base = match bases.as_slice() {
        [base] => *base,
        [] => {
            return Err(bad(
                "none of the APKs is a base APK; give the base with its splits".to_string(),
            ));
        }
        [first, second, ..] => {
            return Err(bad(format!(
                "{} and {} are both base APKs; give one app's base with its splits",
                parts[*first].origin.escape_debug(),
                parts[*second].origin.escape_debug()
            )));
        }
    };
    let base_manifest = &parts[base].apk.manifest;
    let package = &base_manifest.package;
    let fingerprints = parts[base].signature.fingerprints();
    let mut names = BTreeMap::new();
    for (index, part) in parts.iter().enumerate() {
        let manifest = &part.apk.manifest;
        let Some(name) = &manifest.split else {
            continue;
        };
        let shown = name.escape_debug();
        if !valid_split_name(name) {
            return Err(bad(format!("bad split name {shown}")));
        }
        if manifest.package != *package {
            return Err(bad(format!(
                "split {shown} is for package {}, not {}",
                manifest.package.escape_debug(),
                package.escape_debug()
            )));
        }
        if manifest.version_code != base_manifest.version_code {
            return Err(bad(format!(
                "split {shown} has versionCode {}, but the base APK has {}",
                manifest
                    .version_code
                    .as_deref()
                    .unwrap_or("none")
                    .escape_debug(),
                base_manifest
                    .version_code
                    .as_deref()
                    .unwrap_or("none")
                    .escape_debug()
            )));
        }
        if part.signature.fingerprints() != fingerprints {
            return Err(bad(format!(
                "split {shown} is signed by a different certificate than the base APK"
            )));
        }
        if names.insert(name.as_str(), index).is_some() {
            return Err(bad(format!("split {shown} is given twice")));
        }
    }

    // Configuration splits by the feature they configure; the base's own under "".
    let mut groups: BTreeMap<&str, Vec<(usize, Kind)>> = BTreeMap::new();
    let mut chosen = Vec::new();
    for (name, index) in &names {
        let manifest = &parts[*index].apk.manifest;
        let kind = kind_of(name, manifest.feature_split);
        if kind == Kind::Feature {
            chosen.push((*index, kind));
            continue;
        }
        let target = manifest.config_for_split.as_deref().unwrap_or_default();
        if !target.is_empty()
            && !names
                .get(target)
                .is_some_and(|feature| parts[*feature].apk.manifest.feature_split)
        {
            return Err(bad(format!(
                "split {} configures feature {}, which is not given",
                name.escape_debug(),
                target.escape_debug()
            )));
        }
        groups.entry(target).or_default().push((*index, kind));
    }
    // One ABI for every group, since one process runs the app's code: this host's, else the one it
    // translates.
    let abis_of = |splits: &[(usize, Kind)]| -> Vec<String> {
        splits
            .iter()
            .filter_map(|(_, kind)| match kind {
                Kind::Abi(abi) => Some(abi.clone()),
                _ => None,
            })
            .collect()
    };
    let with_abis: Vec<Vec<String>> = groups
        .values()
        .map(|splits| abis_of(splits))
        .filter(|abis| !abis.is_empty())
        .collect();
    let fits_all = |abi: &str| with_abis.iter().all(|abis| abis.iter().any(|a| a == abi));
    let abi = if fits_all(&host.abi) {
        host.abi.as_str()
    } else if let Some(guest) = host.translated.as_deref().filter(|guest| fits_all(guest)) {
        guest
    } else {
        let abis = with_abis
            .iter()
            .find(|abis| !abis.contains(&host.abi))
            .map(|abis| abis.join(", "))
            .unwrap_or_default();
        return Err(bad(format!(
            "this app has native code only for {abis}, and running it on {} needs translation, which is not built yet",
            host.abi
        )));
    };
    let mut skipped = Vec::new();
    for splits in groups.values() {
        let densities: BTreeSet<u32> = splits
            .iter()
            .filter_map(|(_, kind)| match kind {
                Kind::Density(dpi) => Some(*dpi),
                _ => None,
            })
            .collect();
        // Android prefers the closest density above the screen's, scaling down, then the highest.
        let density = densities
            .range(host.density..)
            .next()
            .or(densities.last())
            .copied();
        for (index, kind) in splits {
            let wanted = match kind {
                Kind::Abi(split_abi) => split_abi == abi,
                Kind::Density(dpi) => Some(*dpi) == density,
                Kind::Language(language) => host.languages.contains(language),
                Kind::Feature | Kind::Other => true,
            };
            if wanted {
                chosen.push((*index, kind.clone()));
            } else {
                let name = parts[*index].apk.manifest.split.clone().unwrap_or_default();
                skipped.push((name, kind.clone()));
            }
        }
    }
    let name_of = |index: usize| parts[index].apk.manifest.split.clone().unwrap_or_default();
    chosen.sort_by_key(|(index, _)| name_of(*index));
    skipped.sort_by(|a, b| a.0.cmp(&b.0));

    let provided: BTreeSet<&str> = chosen
        .iter()
        .filter_map(|(index, _)| parts[*index].apk.manifest.split_types.as_deref())
        .flat_map(|types| types.split(','))
        .map(str::trim)
        .collect();
    let required = base_manifest.required_split_types.as_deref().unwrap_or("");
    if let Some(missing) = required
        .split(',')
        .map(str::trim)
        .find(|kind| !kind.is_empty() && !provided.contains(kind))
    {
        return Err(bad(format!(
            "{} needs a split of type {}, which none of the chosen splits provides",
            package.escape_debug(),
            missing.escape_debug()
        )));
    }
    if base_manifest.split_required && names.is_empty() {
        return Err(bad(format!(
            "{} is delivered as split APKs and needs them: give the base APK with its splits, or the bundle",
            package.escape_debug()
        )));
    }
    Ok(Plan {
        base,
        chosen,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_names_tell_what_they_configure() {
        assert_eq!(
            kind_of("config.arm64_v8a", false),
            Kind::Abi("arm64-v8a".into())
        );
        assert_eq!(
            kind_of("feature.config.x86_64", false),
            Kind::Abi("x86_64".into())
        );
        assert_eq!(kind_of("config.xxhdpi", false), Kind::Density(480));
        assert_eq!(kind_of("config.pt_BR", false), Kind::Language("pt".into()));
        assert_eq!(kind_of("config.fil", false), Kind::Language("fil".into()));
        assert_eq!(kind_of("config.astc", false), Kind::Other);
        assert_eq!(kind_of("extras", false), Kind::Other);
        assert_eq!(kind_of("config.en", true), Kind::Feature);
    }

    #[test]
    fn crafted_tables_of_contents_fail_cleanly() {
        for table in [
            &[0x0a, 0x05, 0x01][..],
            &[
                0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
            ],
            &[0x0b],
            &[],
        ] {
            assert!(variant_paths(table).is_err(), "{table:?}");
        }
    }
}
