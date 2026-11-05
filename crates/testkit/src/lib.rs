//! Builds synthetic APKs so tests do not depend on binary samples or the Android SDK.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;

pub const ATTR_NAME: u32 = 0x0101_0003;
pub const ATTR_TARGET_ACTIVITY: u32 = 0x0101_0202;
pub const ATTR_VERSION_CODE: u32 = 0x0101_021b;
pub const ATTR_VERSION_NAME: u32 = 0x0101_021c;
pub const ATTR_MIN_SDK: u32 = 0x0101_020c;
pub const ATTR_TARGET_SDK: u32 = 0x0101_0270;
pub const ATTR_IS_FEATURE_SPLIT: u32 = 0x0101_055b;
pub const ATTR_IS_SPLIT_REQUIRED: u32 = 0x0101_0591;
pub const ATTR_REQUIRED_SPLIT_TYPES: u32 = 0x0101_064e;
pub const ATTR_SPLIT_TYPES: u32 = 0x0101_064f;

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

    /// Changes the contents of an entry already added, as tampering after signing does.
    pub fn replace(mut self, name: &str, bytes: &[u8]) -> Self {
        for entry in self.entries.iter_mut().filter(|(entry, _)| entry == name) {
            entry.1 = bytes.to_vec();
        }
        self
    }

    pub fn write(&self, path: &Path) {
        std::fs::write(path, self.bytes()).unwrap();
    }

    /// The archive, unsigned.
    pub fn bytes(&self) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in &self.entries {
            archive
                .start_file(name.as_str(), SimpleFileOptions::default())
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    /// The archive signed with APK Signature Scheme v2 by `key`, as apksigner signs it.
    pub fn signed_bytes(&self, key: &Key) -> Vec<u8> {
        signing::sign_v2(&self.bytes(), key)
    }

    pub fn write_signed(&self, path: &Path, key: &Key) {
        std::fs::write(path, self.signed_bytes(key)).unwrap();
    }

    /// Adds a JAR signature (v1) by `key`. `apk_signed` is the `X-Android-APK-Signed` value of the
    /// signature file, which names the stronger schemes the APK was also signed with.
    pub fn with_v1_signature(self, key: &Key, apk_signed: Option<&str>) -> Self {
        signing::sign_v1(self, key, apk_signed)
    }
}

pub use signing::Key;

/// The manifest of a split APK of `package`: `split` names it, `config_for` is the feature split
/// a configuration split belongs to.
pub fn split_manifest(
    package: &'static str,
    version_code: u32,
    split: &'static str,
    config_for: Option<&'static str>,
    feature: bool,
) -> Axml {
    let mut attrs = vec![
        plain("package", Value::Str(package)),
        android("versionCode", ATTR_VERSION_CODE, Value::Int(version_code)),
        plain("split", Value::Str(split)),
    ];
    if let Some(feature) = config_for {
        attrs.push(plain("configForSplit", Value::Str(feature)));
    }
    if feature {
        attrs.push(android(
            "isFeatureSplit",
            ATTR_IS_FEATURE_SPLIT,
            Value::Int(0xffff_ffff),
        ));
    }
    Axml::new()
        .start("manifest", &attrs)
        .start("application", &[])
        .end("application")
        .end("manifest")
}

/// bundletool's table of contents, `toc.pb`: one variant per `(minimum SDK, APK paths)`, encoded as
/// its `BuildApksResult` message is (variant 1, targeting 1, SDK targeting 1, value 1, min 1;
/// APK set 2, APK description 2, path 2).
pub fn apks_table(package: &str, variants: &[(u32, Vec<String>)]) -> Vec<u8> {
    fn field(number: u32, content: &[u8]) -> Vec<u8> {
        let mut out = varint(u64::from(number << 3 | 2));
        out.extend(varint(content.len() as u64));
        out.extend_from_slice(content);
        out
    }
    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }
    let mut table = Vec::new();
    for (min_sdk, paths) in variants {
        let mut min = varint(1 << 3);
        min.extend(varint(u64::from(*min_sdk)));
        let targeting = field(1, &field(1, &field(1, &field(1, &min))));
        let descriptions: Vec<u8> = paths
            .iter()
            .flat_map(|path| field(2, &field(2, path.as_bytes())))
            .collect();
        table.extend(field(1, &[targeting, field(2, &descriptions)].concat()));
    }
    table.extend(field(4, package.as_bytes()));
    table
}

mod signing;

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
