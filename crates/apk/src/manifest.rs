//! Parser for the binary XML format of `AndroidManifest.xml`.
//!
//! Every offset comes from untrusted input, so all reads are bounds-checked. Strings stay
//! undecoded until the end: a hostile file can reuse one huge string from every element.

use crate::Error;

const CHUNK_DOCUMENT: u16 = 0x0003;
const CHUNK_STRING_POOL: u16 = 0x0001;
const CHUNK_RESOURCE_MAP: u16 = 0x0180;
const CHUNK_START_ELEMENT: u16 = 0x0102;
const CHUNK_END_ELEMENT: u16 = 0x0103;

const CHUNK_HEADER_SIZE: usize = 8;
const MIN_ATTRIBUTE_SIZE: usize = 20;
const POOL_FLAG_UTF8: u32 = 1 << 8;
const NO_INDEX: u32 = 0xFFFF_FFFF;

const VALUE_REFERENCE: u8 = 0x01;
const VALUE_STRING: u8 = 0x03;

const ATTR_NAME: u32 = 0x0101_0003;
const ATTR_TARGET_ACTIVITY: u32 = 0x0101_0202;
const ATTR_VERSION_CODE: u32 = 0x0101_021b;
const ATTR_VERSION_NAME: u32 = 0x0101_021c;
const ATTR_MIN_SDK: u32 = 0x0101_020c;
const ATTR_TARGET_SDK: u32 = 0x0101_0270;

const ACTION_MAIN: &str = "android.intent.action.MAIN";
const CATEGORY_LAUNCHER: &str = "android.intent.category.LAUNCHER";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub package: String,
    pub version_code: Option<String>,
    pub version_name: Option<String>,
    pub min_sdk: Option<String>,
    pub target_sdk: Option<String>,
    pub launcher: Option<String>,
}

/// Extracts what uxdroid needs from a compiled `AndroidManifest.xml`.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, Error> {
    if u16_at(bytes, 0)? != CHUNK_DOCUMENT {
        return Err(malformed("not binary XML"));
    }
    let header_size = usize::from(u16_at(bytes, 2)?);
    let document_size = u32_at(bytes, 4)? as usize;
    let document = bytes
        .get(..document_size)
        .ok_or(malformed("truncated document"))?;

    let mut pool = None;
    let mut resource_ids: &[u8] = &[];
    let mut walker = Walker::default();

    let mut position = header_size;
    while document.len().saturating_sub(position) >= CHUNK_HEADER_SIZE {
        let kind = u16_at(document, position)?;
        let chunk_header = usize::from(u16_at(document, position + 2)?);
        let size = u32_at(document, position + 4)? as usize;
        // A zero-sized chunk would never advance the walk.
        if size < CHUNK_HEADER_SIZE {
            return Err(malformed("chunk size too small"));
        }
        let chunk = slice(document, position, size)?;

        match kind {
            CHUNK_STRING_POOL => pool = Some(StringPool::new(chunk, chunk_header)?),
            CHUNK_RESOURCE_MAP => {
                resource_ids = chunk
                    .get(chunk_header..)
                    .ok_or(malformed("truncated resource map"))?;
            }
            CHUNK_START_ELEMENT | CHUNK_END_ELEMENT => {
                let pool = pool
                    .as_ref()
                    .ok_or(malformed("element before string pool"))?;
                let names = Names { pool, resource_ids };
                let extension = chunk
                    .get(chunk_header..)
                    .ok_or(malformed("truncated element"))?;
                if kind == CHUNK_START_ELEMENT {
                    walker.start_element(extension, &names)?;
                } else {
                    walker.end_element(extension, &names)?;
                }
            }
            _ => {}
        }
        position += size;
    }

    walker.finish(pool.as_ref())
}

/// An attribute value that has not been decoded yet.
#[derive(Clone, Copy)]
enum AttrValue {
    Text(u32),
    Reference(u32),
    Number(u32),
}

impl AttrValue {
    fn decode(self, pool: &StringPool) -> Result<String, Error> {
        match self {
            AttrValue::Text(index) => pool.get(index),
            AttrValue::Reference(id) => Ok(format!("@{id:#010x}")),
            AttrValue::Number(number) => Ok(number.to_string()),
        }
    }

    fn is_text(self, pool: &StringPool, text: &str) -> Result<bool, Error> {
        match self {
            AttrValue::Text(index) => pool.is(index, text),
            _ => Ok(false),
        }
    }
}

#[derive(Default)]
struct Walker {
    package: Option<AttrValue>,
    version_code: Option<AttrValue>,
    version_name: Option<AttrValue>,
    min_sdk: Option<AttrValue>,
    target_sdk: Option<AttrValue>,
    launcher: Option<AttrValue>,
    component: Option<AttrValue>,
    has_main_action: bool,
    has_launcher_category: bool,
}

impl Walker {
    fn start_element(&mut self, element: &[u8], names: &Names) -> Result<(), Error> {
        let attrs = Attributes::new(element, names)?;
        let pool = names.pool;
        let name = u32_at(element, 4)?;

        if pool.is(name, "manifest")? {
            self.package = attrs.plain("package")?;
            self.version_code = attrs.android(ATTR_VERSION_CODE, "versionCode")?;
            self.version_name = attrs.android(ATTR_VERSION_NAME, "versionName")?;
        } else if pool.is(name, "uses-sdk")? {
            self.min_sdk = attrs.android(ATTR_MIN_SDK, "minSdkVersion")?;
            self.target_sdk = attrs.android(ATTR_TARGET_SDK, "targetSdkVersion")?;
        } else if pool.is(name, "activity")? {
            self.component = attrs.android(ATTR_NAME, "name")?;
        } else if pool.is(name, "activity-alias")? {
            // Launching an alias starts its target, so the target is what must run.
            self.component = match attrs.android(ATTR_TARGET_ACTIVITY, "targetActivity")? {
                Some(target) => Some(target),
                None => attrs.android(ATTR_NAME, "name")?,
            };
        } else if pool.is(name, "intent-filter")? {
            self.has_main_action = false;
            self.has_launcher_category = false;
        } else if pool.is(name, "action")? {
            if let Some(value) = attrs.android(ATTR_NAME, "name")? {
                self.has_main_action |= value.is_text(pool, ACTION_MAIN)?;
            }
        } else if pool.is(name, "category")?
            && let Some(value) = attrs.android(ATTR_NAME, "name")?
        {
            self.has_launcher_category |= value.is_text(pool, CATEGORY_LAUNCHER)?;
        }
        Ok(())
    }

    fn end_element(&mut self, element: &[u8], names: &Names) -> Result<(), Error> {
        let pool = names.pool;
        let name = u32_at(element, 4)?;
        if pool.is(name, "intent-filter")? {
            if self.has_main_action && self.has_launcher_category && self.launcher.is_none() {
                self.launcher = self.component;
            }
        } else if pool.is(name, "activity")? || pool.is(name, "activity-alias")? {
            self.component = None;
        }
        Ok(())
    }

    fn finish(self, pool: Option<&StringPool>) -> Result<Manifest, Error> {
        let (Some(package), Some(pool)) = (self.package, pool) else {
            return Err(malformed("no package name"));
        };
        let decode = |value: Option<AttrValue>| value.map(|value| value.decode(pool)).transpose();

        let package = package.decode(pool)?;
        let launcher = decode(self.launcher)?.map(|name| qualify_class(&package, &name));
        Ok(Manifest {
            package,
            version_code: decode(self.version_code)?,
            version_name: decode(self.version_name)?,
            min_sdk: decode(self.min_sdk)?,
            target_sdk: decode(self.target_sdk)?,
            launcher,
        })
    }
}

/// Expands the manifest's shorthand class names (`.Main`, `Main`) to full names.
fn qualify_class(package: &str, name: &str) -> String {
    if name.starts_with('.') {
        format!("{package}{name}")
    } else if name.contains('.') {
        name.to_string()
    } else {
        format!("{package}.{name}")
    }
}

struct Names<'a> {
    pool: &'a StringPool<'a>,
    resource_ids: &'a [u8],
}

impl Names<'_> {
    /// Build tools may blank attribute names, leaving the resource id as the only identity.
    fn resource_id(&self, name_index: u32) -> Option<u32> {
        let offset = (name_index as usize).checked_mul(4)?;
        u32_at(self.resource_ids, offset).ok().filter(|id| *id != 0)
    }
}

struct Attributes<'a> {
    element: &'a [u8],
    names: &'a Names<'a>,
    start: usize,
    size: usize,
    count: usize,
}

impl<'a> Attributes<'a> {
    fn new(element: &'a [u8], names: &'a Names<'a>) -> Result<Self, Error> {
        let size = usize::from(u16_at(element, 10)?);
        if size < MIN_ATTRIBUTE_SIZE {
            return Err(malformed("attribute size too small"));
        }
        Ok(Self {
            element,
            names,
            start: usize::from(u16_at(element, 8)?),
            size,
            count: usize::from(u16_at(element, 12)?),
        })
    }

    fn android(&self, id: u32, name: &str) -> Result<Option<AttrValue>, Error> {
        for attr in self.offsets() {
            let name_index = u32_at(self.element, attr + 4)?;
            let matches = match self.names.resource_id(name_index) {
                Some(attr_id) => attr_id == id,
                None => self.names.pool.is(name_index, name)?,
            };
            if matches {
                return self.value(attr).map(Some);
            }
        }
        Ok(None)
    }

    fn plain(&self, name: &str) -> Result<Option<AttrValue>, Error> {
        for attr in self.offsets() {
            let name_index = u32_at(self.element, attr + 4)?;
            if self.names.resource_id(name_index).is_none()
                && self.names.pool.is(name_index, name)?
            {
                return self.value(attr).map(Some);
            }
        }
        Ok(None)
    }

    fn offsets(&self) -> impl Iterator<Item = usize> + use<> {
        let (start, size) = (self.start, self.size);
        (0..self.count).map(move |index| start + index * size)
    }

    fn value(&self, attr: usize) -> Result<AttrValue, Error> {
        let raw = u32_at(self.element, attr + 8)?;
        if raw != NO_INDEX {
            return Ok(AttrValue::Text(raw));
        }
        let kind = *self
            .element
            .get(attr + 15)
            .ok_or(malformed("truncated attribute"))?;
        let data = u32_at(self.element, attr + 16)?;
        Ok(match kind {
            VALUE_STRING => AttrValue::Text(data),
            VALUE_REFERENCE => AttrValue::Reference(data),
            _ => AttrValue::Number(data),
        })
    }
}

struct StringPool<'a> {
    chunk: &'a [u8],
    offsets: usize,
    strings: usize,
    count: u32,
    utf8: bool,
}

impl<'a> StringPool<'a> {
    fn new(chunk: &'a [u8], header_size: usize) -> Result<Self, Error> {
        Ok(Self {
            chunk,
            offsets: header_size,
            count: u32_at(chunk, 8)?,
            utf8: u32_at(chunk, 16)? & POOL_FLAG_UTF8 != 0,
            strings: u32_at(chunk, 20)? as usize,
        })
    }

    fn get(&self, index: u32) -> Result<String, Error> {
        let bytes = self.raw(index)?;
        if self.utf8 {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        } else {
            let (units, _) = bytes.as_chunks::<2>();
            let units: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
            Ok(String::from_utf16_lossy(&units))
        }
    }

    /// Compares an entry to ASCII text in time bounded by the text, not by the entry.
    fn is(&self, index: u32, text: &str) -> Result<bool, Error> {
        let bytes = self.raw(index)?;
        if self.utf8 {
            return Ok(bytes == text.as_bytes());
        }
        if bytes.len() != text.len() * 2 {
            return Ok(false);
        }
        let (units, _) = bytes.as_chunks::<2>();
        Ok(units
            .iter()
            .zip(text.bytes())
            .all(|(unit, byte)| *unit == [byte, 0]))
    }

    /// The encoded characters of an entry, without its length prefix.
    fn raw(&self, index: u32) -> Result<&'a [u8], Error> {
        if index >= self.count {
            return Err(malformed("string index out of range"));
        }
        let entry = (index as usize)
            .checked_mul(4)
            .and_then(|offset| offset.checked_add(self.offsets))
            .ok_or(malformed("string index out of range"))?;
        let start = self
            .strings
            .checked_add(u32_at(self.chunk, entry)? as usize)
            .ok_or(malformed("string offset out of range"))?;
        if self.utf8 {
            // UTF-8 entries store the length in UTF-16 units, then in bytes.
            let (_, after_units) = self.utf8_length(start)?;
            let (length, data) = self.utf8_length(after_units)?;
            slice(self.chunk, data, length)
        } else {
            let first = usize::from(u16_at(self.chunk, start)?);
            let data = start
                .checked_add(2)
                .ok_or(malformed("string offset out of range"))?;
            if first & 0x8000 == 0 {
                return slice(self.chunk, data, first * 2);
            }
            let second = usize::from(u16_at(self.chunk, data)?);
            let length = (((first & 0x7FFF) as u64) << 16 | second as u64) * 2;
            let length = usize::try_from(length).map_err(|_| malformed("truncated string"))?;
            slice(self.chunk, data + 2, length)
        }
    }

    fn utf8_length(&self, position: usize) -> Result<(usize, usize), Error> {
        let bytes = self
            .chunk
            .get(position..)
            .ok_or(malformed("truncated string"))?;
        match bytes {
            [first, ..] if first & 0x80 == 0 => Ok((usize::from(*first), position + 1)),
            [first, second, ..] => Ok((
                (usize::from(first & 0x7F) << 8) | usize::from(*second),
                position + 2,
            )),
            _ => Err(malformed("truncated string")),
        }
    }
}

fn malformed(reason: &'static str) -> Error {
    Error::MalformedManifest(reason)
}

fn slice(bytes: &[u8], start: usize, length: usize) -> Result<&[u8], Error> {
    start
        .checked_add(length)
        .and_then(|end| bytes.get(start..end))
        .ok_or(malformed("unexpected end of data"))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let raw = slice(bytes, offset, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let raw = slice(bytes, offset, 4)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}
