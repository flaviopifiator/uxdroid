//! Builds synthetic APKs so tests do not depend on binary samples or the Android SDK.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;

pub const ATTR_NAME: u32 = 0x0101_0003;
pub const ATTR_TARGET_ACTIVITY: u32 = 0x0101_0202;
pub const ATTR_VERSION_CODE: u32 = 0x0101_021b;
pub const ATTR_VERSION_NAME: u32 = 0x0101_021c;
pub const ATTR_MIN_SDK: u32 = 0x0101_020c;
pub const ATTR_TARGET_SDK: u32 = 0x0101_0270;

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";
const NONE: u32 = 0xFFFF_FFFF;

#[derive(Clone)]
pub enum Value {
    Str(&'static str),
    Int(u32),
    Ref(u32),
}

#[derive(Clone)]
pub struct Attr {
    pub name: &'static str,
    pub res_id: Option<u32>,
    pub value: Value,
}

/// An `android:*` attribute, identified by its framework resource id.
pub fn android(name: &'static str, res_id: u32, value: Value) -> Attr {
    Attr {
        name,
        res_id: Some(res_id),
        value,
    }
}

/// An attribute without a namespace, such as `package`.
pub fn plain(name: &'static str, value: Value) -> Attr {
    Attr {
        name,
        res_id: None,
        value,
    }
}

enum Event {
    Start(&'static str, Vec<Attr>),
    End(&'static str),
}

/// Encodes a binary XML document the way aapt2 does.
#[derive(Default)]
pub struct Axml {
    events: Vec<Event>,
    utf16: bool,
    strip_attr_names: bool,
}

impl Axml {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the string pool as UTF-16, as older build tools do.
    pub fn utf16(mut self) -> Self {
        self.utf16 = true;
        self
    }

    /// Blanks attribute names, leaving only resource ids, as resource shrinkers do.
    pub fn strip_attr_names(mut self) -> Self {
        self.strip_attr_names = true;
        self
    }

    pub fn start(mut self, name: &'static str, attrs: &[Attr]) -> Self {
        self.events.push(Event::Start(name, attrs.to_vec()));
        self
    }

    pub fn end(mut self, name: &'static str) -> Self {
        self.events.push(Event::End(name));
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut res_ids = Vec::new();
        let mut attr_slots = HashMap::new();
        for attr in self.attrs().filter(|a| a.res_id.is_some()) {
            let id = attr.res_id.unwrap();
            attr_slots.entry(id).or_insert_with(|| {
                strings.push(if self.strip_attr_names { "" } else { attr.name }.to_string());
                res_ids.push(id);
                strings.len() as u32 - 1
            });
        }

        let mut interned = HashMap::new();
        let mut intern = |strings: &mut Vec<String>, text: &str| -> u32 {
            if let Some(index) = interned.get(text) {
                return *index;
            }
            strings.push(text.to_string());
            let index = strings.len() as u32 - 1;
            interned.insert(text.to_string(), index);
            index
        };

        let ns_uri = intern(&mut strings, ANDROID_NS);
        let ns_prefix = intern(&mut strings, "android");

        let mut body = Vec::new();
        push_namespace(&mut body, 0x0100, ns_prefix, ns_uri);
        for event in &self.events {
            match event {
                Event::Start(name, attrs) => {
                    let name = intern(&mut strings, name);
                    let mut chunk = Vec::new();
                    push_u32(&mut chunk, 1);
                    push_u32(&mut chunk, NONE);
                    push_u32(&mut chunk, NONE);
                    push_u32(&mut chunk, name);
                    push_u16(&mut chunk, 20);
                    push_u16(&mut chunk, 20);
                    push_u16(&mut chunk, attrs.len() as u16);
                    chunk.extend_from_slice(&[0; 6]);
                    for attr in attrs {
                        let (ns, attr_name) = match attr.res_id {
                            Some(id) => (ns_uri, attr_slots[&id]),
                            None => (NONE, intern(&mut strings, attr.name)),
                        };
                        let (raw, kind, data) = match attr.value {
                            Value::Str(text) => {
                                let index = intern(&mut strings, text);
                                (index, 0x03, index)
                            }
                            Value::Int(number) => (NONE, 0x10, number),
                            Value::Ref(id) => (NONE, 0x01, id),
                        };
                        push_u32(&mut chunk, ns);
                        push_u32(&mut chunk, attr_name);
                        push_u32(&mut chunk, raw);
                        push_u16(&mut chunk, 8);
                        chunk.push(0);
                        chunk.push(kind);
                        push_u32(&mut chunk, data);
                    }
                    push_chunk(&mut body, 0x0102, 16, &chunk);
                }
                Event::End(name) => {
                    let name = intern(&mut strings, name);
                    let mut chunk = Vec::new();
                    push_u32(&mut chunk, 1);
                    push_u32(&mut chunk, NONE);
                    push_u32(&mut chunk, NONE);
                    push_u32(&mut chunk, name);
                    push_chunk(&mut body, 0x0103, 16, &chunk);
                }
            }
        }
        push_namespace(&mut body, 0x0101, ns_prefix, ns_uri);

        let mut content = Vec::new();
        push_string_pool(&mut content, &strings, self.utf16);
        let mut map = Vec::new();
        res_ids.iter().for_each(|id| push_u32(&mut map, *id));
        push_chunk(&mut content, 0x0180, 8, &map);
        content.extend_from_slice(&body);

        let mut document = Vec::new();
        push_chunk(&mut document, 0x0003, 8, &content);
        document
    }

    fn attrs(&self) -> impl Iterator<Item = &Attr> {
        self.events.iter().flat_map(|event| match event {
            Event::Start(_, attrs) => attrs.as_slice(),
            Event::End(_) => &[],
        })
    }
}

/// A typical manifest: a plain activity followed by the launcher one.
pub fn sample_manifest() -> Axml {
    Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str("com.example.app")),
                android("versionCode", ATTR_VERSION_CODE, Value::Int(42)),
                android("versionName", ATTR_VERSION_NAME, Value::Str("1.2.3")),
            ],
        )
        .start(
            "uses-sdk",
            &[
                android("minSdkVersion", ATTR_MIN_SDK, Value::Int(21)),
                android("targetSdkVersion", ATTR_TARGET_SDK, Value::Int(34)),
            ],
        )
        .end("uses-sdk")
        .start("application", &[])
        .start(
            "activity",
            &[android("name", ATTR_NAME, Value::Str(".SettingsActivity"))],
        )
        .end("activity")
        .start(
            "activity",
            &[android("name", ATTR_NAME, Value::Str(".MainActivity"))],
        )
        .start("intent-filter", &[])
        .start(
            "action",
            &[android(
                "name",
                ATTR_NAME,
                Value::Str("android.intent.action.MAIN"),
            )],
        )
        .end("action")
        .start(
            "category",
            &[android(
                "name",
                ATTR_NAME,
                Value::Str("android.intent.category.LAUNCHER"),
            )],
        )
        .end("category")
        .end("intent-filter")
        .end("activity")
        .end("application")
        .end("manifest")
}

/// A valid DEX header followed by no data, which is all `inspect` reads.
pub fn dex(classes: u32, methods: u32) -> Vec<u8> {
    let mut header = vec![0u8; 112];
    header[..8].copy_from_slice(b"dex\n039\0");
    header[88..92].copy_from_slice(&methods.to_le_bytes());
    header[96..100].copy_from_slice(&classes.to_le_bytes());
    header
}

#[derive(Default)]
pub struct Apk {
    entries: Vec<(String, Vec<u8>)>,
}

impl Apk {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn manifest(self, manifest: &Axml) -> Self {
        self.file("AndroidManifest.xml", &manifest.encode())
    }

    pub fn native_lib(self, abi: &str, name: &str) -> Self {
        self.file(&format!("lib/{abi}/{name}"), b"\x7fELF")
    }

    pub fn file(mut self, name: &str, bytes: &[u8]) -> Self {
        self.entries.push((name.to_string(), bytes.to_vec()));
        self
    }

    pub fn write(&self, path: &Path) {
        let mut archive = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in &self.entries {
            archive
                .start_file(name.as_str(), SimpleFileOptions::default())
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
    }
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// `payload` holds everything after the 8-byte chunk header, extended header included.
fn push_chunk(out: &mut Vec<u8>, kind: u16, header_size: u16, payload: &[u8]) {
    push_u16(out, kind);
    push_u16(out, header_size);
    push_u32(out, 8 + payload.len() as u32);
    out.extend_from_slice(payload);
}

fn push_namespace(out: &mut Vec<u8>, kind: u16, prefix: u32, uri: u32) {
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 1);
    push_u32(&mut chunk, NONE);
    push_u32(&mut chunk, prefix);
    push_u32(&mut chunk, uri);
    push_chunk(out, kind, 16, &chunk);
}

fn push_string_pool(out: &mut Vec<u8>, strings: &[String], utf16: bool) {
    let mut data = Vec::new();
    let mut offsets = Vec::new();
    for text in strings {
        offsets.push(data.len() as u32);
        if utf16 {
            let units: Vec<u16> = text.encode_utf16().collect();
            if units.len() >= 0x8000 {
                push_u16(&mut data, 0x8000 | (units.len() >> 16) as u16);
            }
            push_u16(&mut data, units.len() as u16);
            units.iter().for_each(|unit| push_u16(&mut data, *unit));
            push_u16(&mut data, 0);
        } else {
            data.push(text.chars().count() as u8);
            data.push(text.len() as u8);
            data.extend_from_slice(text.as_bytes());
            data.push(0);
        }
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }

    let mut chunk = Vec::new();
    push_u32(&mut chunk, strings.len() as u32);
    push_u32(&mut chunk, 0);
    push_u32(&mut chunk, if utf16 { 0 } else { 1 << 8 });
    push_u32(&mut chunk, 28 + 4 * strings.len() as u32);
    push_u32(&mut chunk, 0);
    offsets
        .iter()
        .for_each(|offset| push_u32(&mut chunk, *offset));
    chunk.extend_from_slice(&data);
    push_chunk(out, 0x0001, 28, &chunk);
}
