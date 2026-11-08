//! Reads `resources.arsc`, the compiled resource table, far enough to resolve the label and icon a
//! launcher shows for an app.
//!
//! Every offset comes from untrusted input, so all reads are bounds-checked, and a table that cannot
//! be read resolves nothing instead of failing: a label or icon is never worth refusing an app over.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::manifest::{Label, Manifest, StringPool, slice, u16_at, u32_at};

const CHUNK_STRING_POOL: u16 = 0x0001;
const CHUNK_TABLE: u16 = 0x0002;
const CHUNK_PACKAGE: u16 = 0x0200;
const CHUNK_TYPE: u16 = 0x0201;
const CHUNK_HEADER_SIZE: usize = 8;

const TYPE_FLAG_SPARSE: u8 = 0x01;
const TYPE_FLAG_OFFSET16: u8 = 0x02;
const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
const ENTRY_FLAG_COMPACT: u16 = 0x0008;
const NO_ENTRY: u32 = 0xFFFF_FFFF;
const NO_ENTRY16: u16 = 0xFFFF;

const VALUE_REFERENCE: u8 = 0x01;
const VALUE_STRING: u8 = 0x03;

/// `ResTable_config` offsets, from the start of the configuration.
const CONFIG_LANGUAGE: usize = 8;
const CONFIG_COUNTRY: usize = 10;
const CONFIG_DENSITY: usize = 14;
const DENSITY_DEFAULT: u16 = 0;
const DENSITY_MEDIUM: u16 = 160;
const DENSITY_ANY: u16 = 0xFFFE;
const DENSITY_NONE: u16 = 0xFFFF;

/// References are followed this deep; a loop of them ends here.
const MAX_REFERENCE_DEPTH: usize = 8;
/// Each lookup walks the table once, so a crafted table of references that fan out is cut off after
/// this many, keeping the work linear in the table's size.
const MAX_LOOKUPS: usize = 256;
/// More locales than any app ships; the limit bounds the work a crafted table can ask for.
const MAX_LABELS: usize = 256;
/// The largest tables in real apps are tens of MiB. The limit stops decompression bombs.
const MAX_TABLE_BYTES: u64 = 64 * 1024 * 1024;
/// Launcher icons are a few KiB to a few hundred; a larger file is not used.
const MAX_ICON_BYTES: u64 = 2 * 1024 * 1024;
const TABLE_ENTRY: &str = "resources.arsc";

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A launcher icon read from an APK, its bytes checked to be the format it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Webp,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Webp => "webp",
        }
    }

    fn of_path(path: &str) -> Option<Self> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".png") {
            Some(ImageFormat::Png)
        } else if lower.ends_with(".webp") {
            Some(ImageFormat::Webp)
        } else {
            None
        }
    }

    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            ImageFormat::Png => bytes.starts_with(PNG_SIGNATURE),
            ImageFormat::Webp => {
                bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
            }
        }
    }
}

/// What a launcher shows for an app: its label in the default locale, the labels of other locales
/// keyed `ll` or `ll_CC`, and its icon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Appearance {
    pub label: Option<String>,
    pub labels: Vec<(String, String)>,
    pub icon: Option<Icon>,
}

/// A raster file an icon resource names in one configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconFile {
    pub path: String,
    pub density: u16,
    pub format: ImageFormat,
}

/// A parsed `resources.arsc`, holding slices of the table's bytes.
pub struct ResourceTable<'a> {
    pool: StringPool<'a>,
    packages: Vec<(u32, Vec<&'a [u8]>)>,
    lookups: std::cell::Cell<usize>,
}

/// One value of a resource in one configuration.
#[derive(Clone, Copy)]
struct Found<'a> {
    config: &'a [u8],
    kind: u8,
    data: u32,
}

impl<'a> ResourceTable<'a> {
    /// Parses the table's chunk structure; `None` when it is not a table.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        if u16_at(bytes, 0).ok()? != CHUNK_TABLE {
            return None;
        }
        let header_size = usize::from(u16_at(bytes, 2).ok()?);
        let size = u32_at(bytes, 4).ok()? as usize;
        let table = bytes.get(..size)?;
        let mut pool = None;
        let mut packages = Vec::new();
        for (kind, chunk_header, chunk) in chunks(table, header_size) {
            match kind {
                CHUNK_STRING_POOL if pool.is_none() => {
                    pool = StringPool::new(chunk, chunk_header).ok();
                }
                CHUNK_PACKAGE => {
                    let id = u32_at(chunk, 8).ok()?;
                    let types = chunks(chunk, chunk_header)
                        .filter(|(kind, _, _)| *kind == CHUNK_TYPE)
                        .map(|(_, _, chunk)| chunk)
                        .collect();
                    packages.push((id, types));
                }
                _ => {}
            }
        }
        Some(Self {
            pool: pool?,
            packages,
            lookups: std::cell::Cell::new(0),
        })
    }

    /// The label `id` names in the default locale, and in every other locale it has.
    pub fn labels(&self, id: u32) -> (Option<String>, Vec<(String, String)>) {
        let mut default = None;
        let mut localized = Vec::new();
        for found in self.find(id) {
            if localized.len() >= MAX_LABELS {
                break;
            }
            let Some(text) = self.text(found, 0) else {
                continue;
            };
            match locale(found.config) {
                Some(None) if default.is_none() => default = Some(text),
                Some(Some(locale)) if !localized.iter().any(|(known, _)| *known == locale) => {
                    localized.push((locale, text));
                }
                _ => {}
            }
        }
        (default, localized)
    }

    /// The raster files the icon `id` names, in every configuration, best first: PNG before WebP,
    /// then higher densities first.
    pub fn icon_files(&self, id: u32) -> Vec<IconFile> {
        let mut files = Vec::new();
        self.collect_files(id, None, 0, &mut files);
        files.sort_by_key(|file| {
            (
                file.format != ImageFormat::Png,
                std::cmp::Reverse(density_rank(file.density)),
            )
        });
        files.dedup_by(|a, b| a.path == b.path);
        files
    }

    fn collect_files(&self, id: u32, density: Option<u16>, depth: usize, out: &mut Vec<IconFile>) {
        if depth > MAX_REFERENCE_DEPTH || out.len() >= MAX_LABELS {
            return;
        }
        for found in self.find(id) {
            // A file of a referenced resource is used at the density the reference was made for.
            let density = density.unwrap_or_else(|| config_density(found.config));
            match found.kind {
                VALUE_STRING => {
                    let Ok(path) = self.pool.get(found.data) else {
                        continue;
                    };
                    if let Some(format) = ImageFormat::of_path(&path) {
                        out.push(IconFile {
                            path,
                            density,
                            format,
                        });
                    }
                }
                VALUE_REFERENCE => {
                    let density = (density != DENSITY_DEFAULT).then_some(density);
                    self.collect_files(found.data, density, depth + 1, out);
                }
                _ => {}
            }
        }
    }

    /// The text of a value, following references to their default-locale value.
    fn text(&self, found: Found, depth: usize) -> Option<String> {
        match found.kind {
            VALUE_STRING => self.pool.get(found.data).ok(),
            VALUE_REFERENCE if depth < MAX_REFERENCE_DEPTH => self
                .find(found.data)
                .find(|target| locale(target.config) == Some(None))
                .and_then(|target| self.text(target, depth + 1)),
            _ => None,
        }
    }

    /// Every value of resource `id`, one per configuration that has it.
    fn find(&self, id: u32) -> impl Iterator<Item = Found<'a>> + '_ {
        let package = id >> 24;
        let kind = ((id >> 16) & 0xFF) as u8;
        let entry = id & 0xFFFF;
        let allowed = self.lookups.get() < MAX_LOOKUPS;
        self.lookups.set(self.lookups.get() + 1);
        self.packages
            .iter()
            .filter(move |(id, _)| allowed && *id == package)
            .flat_map(|(_, types)| types.iter())
            .filter(move |chunk| kind != 0 && chunk.get(8) == Some(&kind))
            .filter_map(move |chunk| entry_value(chunk, entry))
    }
}

/// The chunks inside `bytes` after its header, as `(type, header size, chunk)`; the walk stops at
/// the first chunk that does not fit.
fn chunks(bytes: &[u8], header_size: usize) -> impl Iterator<Item = (u16, usize, &[u8])> {
    let mut position = header_size;
    std::iter::from_fn(move || {
        if bytes.len().checked_sub(position)? < CHUNK_HEADER_SIZE {
            return None;
        }
        let kind = u16_at(bytes, position).ok()?;
        let chunk_header = usize::from(u16_at(bytes, position + 2).ok()?);
        let size = u32_at(bytes, position + 4).ok()? as usize;
        // A chunk under its own header's size would never advance the walk.
        if size < CHUNK_HEADER_SIZE {
            return None;
        }
        let chunk = slice(bytes, position, size).ok()?;
        position = position.checked_add(size)?;
        Some((kind, chunk_header, chunk))
    })
}

/// The value of entry `entry` in a type chunk, with the chunk's configuration.
fn entry_value(chunk: &[u8], entry: u32) -> Option<Found<'_>> {
    let header_size = usize::from(u16_at(chunk, 2).ok()?);
    let flags = *chunk.get(9)?;
    let count = u32_at(chunk, 12).ok()?;
    let entries_start = u32_at(chunk, 16).ok()? as usize;
    let config = chunk.get(20..header_size)?;
    let offset = if flags & TYPE_FLAG_SPARSE != 0 {
        // Pairs of (entry, offset / 4), sorted by entry.
        (0..count as usize).find_map(|index| {
            let at = header_size.checked_add(index.checked_mul(4)?)?;
            (u32::from(u16_at(chunk, at).ok()?) == entry)
                .then(|| {
                    u16_at(chunk, at + 2)
                        .ok()
                        .map(|offset| usize::from(offset) * 4)
                })
                .flatten()
        })?
    } else {
        if entry >= count {
            return None;
        }
        if flags & TYPE_FLAG_OFFSET16 != 0 {
            let raw = u16_at(chunk, header_size.checked_add(entry as usize * 2)?).ok()?;
            if raw == NO_ENTRY16 {
                return None;
            }
            usize::from(raw) * 4
        } else {
            let raw = u32_at(chunk, header_size.checked_add(entry as usize * 4)?).ok()?;
            if raw == NO_ENTRY {
                return None;
            }
            raw as usize
        }
    };
    let at = entries_start.checked_add(offset)?;
    let size = u16_at(chunk, at).ok()?;
    let entry_flags = u16_at(chunk, at + 2).ok()?;
    let (kind, data) = if entry_flags & ENTRY_FLAG_COMPACT != 0 {
        ((entry_flags >> 8) as u8, u32_at(chunk, at + 4).ok()?)
    } else if entry_flags & ENTRY_FLAG_COMPLEX != 0 {
        return None;
    } else {
        let value = at.checked_add(usize::from(size))?;
        (
            *chunk.get(value.checked_add(3)?)?,
            u32_at(chunk, value + 4).ok()?,
        )
    };
    Some(Found { config, kind, data })
}

/// The locale of a configuration: `Some(None)` for the default, `Some(Some("ll_CC"))` for a two-letter
/// language and optional region, and `None` for any locale a desktop entry cannot name.
fn locale(config: &[u8]) -> Option<Option<String>> {
    let pair = |at: usize| -> [u8; 2] {
        match config.get(at..at + 2) {
            Some([a, b]) => [*a, *b],
            _ => [0, 0],
        }
    };
    let language = pair(CONFIG_LANGUAGE);
    let country = pair(CONFIG_COUNTRY);
    match (language, country) {
        ([0, 0], [0, 0]) => Some(None),
        (language, [0, 0]) if language.iter().all(u8::is_ascii_lowercase) => {
            Some(Some(String::from_utf8_lossy(&language).into_owned()))
        }
        (language, country)
            if language.iter().all(u8::is_ascii_lowercase)
                && country.iter().all(u8::is_ascii_uppercase) =>
        {
            Some(Some(format!(
                "{}_{}",
                String::from_utf8_lossy(&language),
                String::from_utf8_lossy(&country)
            )))
        }
        _ => None,
    }
}

fn config_density(config: &[u8]) -> u16 {
    config
        .get(CONFIG_DENSITY..CONFIG_DENSITY + 2)
        .map_or(DENSITY_DEFAULT, |raw| u16::from_le_bytes([raw[0], raw[1]]))
}

/// How sharp a density's files are: the default is medium, and a file for no density in particular
/// ranks below every density.
fn density_rank(density: u16) -> u16 {
    match density {
        DENSITY_DEFAULT => DENSITY_MEDIUM,
        DENSITY_ANY | DENSITY_NONE => 1,
        density => density,
    }
}

/// The label and icon a launcher shows for the app of `manifest`, read from its base APK and then
/// its splits, which may hold the icon's other densities and the label's other locales. What cannot
/// be read is left out.
pub fn appearance(manifest: &Manifest, apks: &[&Path]) -> Appearance {
    let mut archives: Vec<ZipArchive<BufReader<File>>> = apks
        .iter()
        .filter_map(|path| {
            let file = File::open(path).ok()?;
            ZipArchive::new(BufReader::new(file)).ok()
        })
        .collect();
    let tables: Vec<Option<Vec<u8>>> = archives
        .iter_mut()
        .map(|archive| read_capped(archive, TABLE_ENTRY, MAX_TABLE_BYTES))
        .collect();
    let parsed: Vec<Option<ResourceTable>> = tables
        .iter()
        .map(|bytes| bytes.as_deref().and_then(ResourceTable::parse))
        .collect();

    let mut look = Appearance::default();
    match &manifest.label {
        Some(Label::Text(text)) => look.label = Some(text.clone()),
        Some(Label::Resource(id)) => {
            for table in parsed.iter().flatten() {
                let (default, localized) = table.labels(*id);
                if look.label.is_none() {
                    look.label = default;
                }
                for (locale, text) in localized {
                    if look.labels.len() < MAX_LABELS
                        && !look.labels.iter().any(|(known, _)| *known == locale)
                    {
                        look.labels.push((locale, text));
                    }
                }
            }
        }
        None => {}
    }

    if let Some(id) = manifest.icon {
        let mut files: Vec<(usize, IconFile)> = parsed
            .iter()
            .enumerate()
            .filter_map(|(index, table)| table.as_ref().map(|table| (index, table)))
            .flat_map(|(index, table)| {
                table
                    .icon_files(id)
                    .into_iter()
                    .map(move |file| (index, file))
            })
            .collect();
        files.sort_by_key(|(_, file)| {
            (
                file.format != ImageFormat::Png,
                std::cmp::Reverse(density_rank(file.density)),
            )
        });
        look.icon = files.into_iter().find_map(|(index, file)| {
            let bytes = read_capped(&mut archives[index], &file.path, MAX_ICON_BYTES)?;
            file.format.matches(&bytes).then_some(Icon {
                bytes,
                format: file.format,
            })
        });
    }
    look
}

/// The whole of entry `name`, or `None` when it is missing, unreadable or larger than `limit`.
fn read_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    name: &str,
    limit: u64,
) -> Option<Vec<u8>> {
    let entry = archive.by_name(name).ok()?;
    if entry.size() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    entry.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}
