//! Parser for the binary XML format of `AndroidManifest.xml`.
//!
//! Every offset comes from untrusted input, so all reads are bounds-checked.

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
    while position + CHUNK_HEADER_SIZE <= document.len() {
        let kind = u16_at(document, position)?;
        let chunk_header = usize::from(u16_at(document, position + 2)?);
        let size = u32_at(document, position + 4)? as usize;
        // A zero-sized chunk would never advance the walk.
        if size < CHUNK_HEADER_SIZE {
            return Err(malformed("chunk size too small"));
        }
        let chunk = document
            .get(position..position + size)
            .ok_or(malformed("truncated chunk"))?;

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

    walker.finish()
}

#[derive(Default)]
struct Walker {
    package: Option<String>,
    version_code: Option<String>,
    version_name: Option<String>,
    min_sdk: Option<String>,
    target_sdk: Option<String>,
    launcher: Option<String>,
    component: Option<String>,
    has_main_action: bool,
    has_launcher_category: bool,
}

impl Walker {
    fn start_element(&mut self, element: &[u8], names: &Names) -> Result<(), Error> {
        let attrs = Attributes::new(element, names)?;
        match names.pool.get(u32_at(element, 4)?)?.as_str() {
            "manifest" => {
                self.package = attrs.plain("package")?;
                self.version_code = attrs.android(ATTR_VERSION_CODE, "versionCode")?;
                self.version_name = attrs.android(ATTR_VERSION_NAME, "versionName")?;
            }
            "uses-sdk" => {
                self.min_sdk = attrs.android(ATTR_MIN_SDK, "minSdkVersion")?;
                self.target_sdk = attrs.android(ATTR_TARGET_SDK, "targetSdkVersion")?;
            }
            "activity" => self.component = attrs.android(ATTR_NAME, "name")?,
            // Launching an alias starts its target, so the target is what must run.
            "activity-alias" => {
                self.component = match attrs.android(ATTR_TARGET_ACTIVITY, "targetActivity")? {
                    Some(target) => Some(target),
                    None => attrs.android(ATTR_NAME, "name")?,
                };
            }
            "intent-filter" => {
                self.has_main_action = false;
                self.has_launcher_category = false;
            }
            "action" => {
                self.has_main_action |=
                    attrs.android(ATTR_NAME, "name")?.as_deref() == Some(ACTION_MAIN);
            }
            "category" => {
                self.has_launcher_category |=
                    attrs.android(ATTR_NAME, "name")?.as_deref() == Some(CATEGORY_LAUNCHER);
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, element: &[u8], names: &Names) -> Result<(), Error> {
        match names.pool.get(u32_at(element, 4)?)?.as_str() {
            "intent-filter" => {
                if self.has_main_action && self.has_launcher_category && self.launcher.is_none() {
                    self.launcher = self.component.clone();
                }
            }
            "activity" | "activity-alias" => self.component = None,
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<Manifest, Error> {
        let package = self.package.ok_or(malformed("no package name"))?;
        let launcher = self.launcher.map(|name| qualify_class(&package, &name));
        Ok(Manifest {
            package,
            version_code: self.version_code,
            version_name: self.version_name,
            min_sdk: self.min_sdk,
            target_sdk: self.target_sdk,
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

    fn android(&self, id: u32, name: &str) -> Result<Option<String>, Error> {
        self.find(|attr_id, attr_name| match attr_id {
            Some(attr_id) => Ok(attr_id == id),
            None => Ok(attr_name()? == name),
        })
    }

    fn plain(&self, name: &str) -> Result<Option<String>, Error> {
        self.find(|attr_id, attr_name| Ok(attr_id.is_none() && attr_name()? == name))
    }

    fn find(
        &self,
        matches: impl Fn(Option<u32>, &dyn Fn() -> Result<String, Error>) -> Result<bool, Error>,
    ) -> Result<Option<String>, Error> {
        for index in 0..self.count {
            let attr = self.start + index * self.size;
            let name_index = u32_at(self.element, attr + 4)?;
            let name = || self.names.pool.get(name_index);
            if matches(self.names.resource_id(name_index), &name)? {
                return self.value(attr).map(Some);
            }
        }
        Ok(None)
    }

    fn value(&self, attr: usize) -> Result<String, Error> {
        let raw = u32_at(self.element, attr + 8)?;
        if raw != NO_INDEX {
            return self.names.pool.get(raw);
        }
        let kind = *self
            .element
            .get(attr + 15)
            .ok_or(malformed("truncated attribute"))?;
        let data = u32_at(self.element, attr + 16)?;
        match kind {
            VALUE_STRING => self.names.pool.get(data),
            VALUE_REFERENCE => Ok(format!("@{data:#010x}")),
            _ => Ok(data.to_string()),
        }
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
        if index >= self.count {
            return Err(malformed("string index out of range"));
        }
        let entry = self.offsets + index as usize * 4;
        let start = self.strings + u32_at(self.chunk, entry)? as usize;
        if self.utf8 {
            self.utf8_at(start)
        } else {
            self.utf16_at(start)
        }
    }

    /// UTF-8 entries store the length in UTF-16 units, then in bytes.
    fn utf8_at(&self, start: usize) -> Result<String, Error> {
        let (_, after_units) = self.utf8_length(start)?;
        let (length, data) = self.utf8_length(after_units)?;
        let bytes = self
            .chunk
            .get(data..data + length)
            .ok_or(malformed("truncated string"))?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn utf8_length(&self, position: usize) -> Result<(usize, usize), Error> {
        let first = usize::from(
            *self
                .chunk
                .get(position)
                .ok_or(malformed("truncated string"))?,
        );
        if first & 0x80 == 0 {
            return Ok((first, position + 1));
        }
        let second = usize::from(
            *self
                .chunk
                .get(position + 1)
                .ok_or(malformed("truncated string"))?,
        );
        Ok((((first & 0x7F) << 8) | second, position + 2))
    }

    fn utf16_at(&self, start: usize) -> Result<String, Error> {
        let first = usize::from(u16_at(self.chunk, start)?);
        let (length, data) = if first & 0x8000 == 0 {
            (first, start + 2)
        } else {
            let second = usize::from(u16_at(self.chunk, start + 2)?);
            (((first & 0x7FFF) << 16) | second, start + 4)
        };
        let bytes = self
            .chunk
            .get(data..data + length * 2)
            .ok_or(malformed("truncated string"))?;
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }
}

fn malformed(reason: &'static str) -> Error {
    Error::MalformedManifest(reason)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(malformed("unexpected end of data"))?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(malformed("unexpected end of data"))?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}
