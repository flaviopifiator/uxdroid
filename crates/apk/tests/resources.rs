//! `resources.arsc`: the label and icon a launcher shows, and damaged tables that must resolve
//! nothing instead of panicking or hanging.

use std::time::{Duration, Instant};

use uxdroid_apk::{ImageFormat, Label, ResourceTable, appearance, parse_manifest};
use uxdroid_testkit::{
    ATTR_ICON, ATTR_LABEL, ATTR_NAME, Apk, Arsc, Axml, Config, Value, android, plain, png,
};

const LABEL: u32 = 0x7f01_0000;
const ICON: u32 = 0x7f02_0000;
const ICON_ALIAS: u32 = 0x7f03_0000;

fn manifest(label: Value, icon: Option<u32>) -> Axml {
    let mut app = vec![android("label", ATTR_LABEL, label)];
    if let Some(icon) = icon {
        app.push(android("icon", ATTR_ICON, Value::Ref(icon)));
    }
    Axml::new()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.app"))],
        )
        .start("application", &app)
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

fn table() -> Arsc {
    Arsc::new()
        .string(LABEL, Config::DEFAULT, "Example")
        .string(LABEL, Config::locale("es", ""), "Ejemplo")
        .string(LABEL, Config::locale("pt", "BR"), "Exemplo")
        .string(
            ICON,
            Config::density(160),
            "res/mipmap-mdpi/ic_launcher.png",
        )
        .string(
            ICON,
            Config::density(480),
            "res/mipmap-xxhdpi/ic_launcher.png",
        )
        .string(
            ICON,
            Config::density(640),
            "res/mipmap-xxxhdpi/ic_launcher.webp",
        )
        .string(
            ICON,
            Config::density(0xFFFE),
            "res/mipmap-anydpi-v26/ic_launcher.xml",
        )
}

fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("res-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn the_manifest_gives_the_label_and_icon_of_the_application() {
    let parsed = parse_manifest(&manifest(Value::Ref(LABEL), Some(ICON)).encode()).unwrap();

    assert_eq!(parsed.label, Some(Label::Resource(LABEL)));
    assert_eq!(parsed.icon, Some(ICON));
}

#[test]
fn a_literal_label_is_the_text_itself() {
    let parsed = parse_manifest(&manifest(Value::Str("Plain"), None).encode()).unwrap();

    assert_eq!(parsed.label, Some(Label::Text("Plain".to_string())));
    assert_eq!(parsed.icon, None);
}

#[test]
fn labels_resolve_in_the_default_and_every_named_locale() {
    let bytes = table().encode();
    let table = ResourceTable::parse(&bytes).unwrap();

    let (default, localized) = table.labels(LABEL);

    assert_eq!(default.as_deref(), Some("Example"));
    assert_eq!(
        localized,
        vec![
            ("es".to_string(), "Ejemplo".to_string()),
            ("pt_BR".to_string(), "Exemplo".to_string())
        ]
    );
}

#[test]
fn icon_files_rank_png_first_then_the_highest_density() {
    for arsc in [table(), table().sparse()] {
        let bytes = arsc.encode();
        let table = ResourceTable::parse(&bytes).unwrap();

        let paths: Vec<String> = table
            .icon_files(ICON)
            .into_iter()
            .map(|file| file.path)
            .collect();

        assert_eq!(
            paths,
            [
                "res/mipmap-xxhdpi/ic_launcher.png",
                "res/mipmap-mdpi/ic_launcher.png",
                "res/mipmap-xxxhdpi/ic_launcher.webp",
            ]
        );
    }
}

#[test]
fn references_are_followed_and_a_loop_of_them_ends() {
    let bytes = Arsc::new()
        .reference(ICON, Config::density(320), ICON_ALIAS)
        .string(ICON_ALIAS, Config::DEFAULT, "res/drawable/icon.png")
        .reference(LABEL, Config::DEFAULT, LABEL)
        .encode();
    let table = ResourceTable::parse(&bytes).unwrap();

    let files = table.icon_files(ICON);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "res/drawable/icon.png");
    assert_eq!(files[0].density, 320);
    assert_eq!(table.labels(LABEL).0, None);
}

#[test]
fn the_appearance_takes_the_sharpest_icon_whose_bytes_are_an_image() {
    let dir = scratch("appearance");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&manifest(Value::Ref(LABEL), Some(ICON)))
        .file("resources.arsc", &table().encode())
        // Named PNG but not one: the next best file is used.
        .file("res/mipmap-xxhdpi/ic_launcher.png", b"not a png")
        .file("res/mipmap-mdpi/ic_launcher.png", &png("mdpi"))
        .write(&apk);
    let parsed = uxdroid_apk::inspect(&apk).unwrap().manifest;

    let look = appearance(&parsed, &[&apk]);

    assert_eq!(look.label.as_deref(), Some("Example"));
    assert_eq!(look.labels.len(), 2);
    let icon = look.icon.unwrap();
    assert_eq!(icon.format, ImageFormat::Png);
    assert_eq!(icon.bytes, png("mdpi"));
}

#[test]
fn a_split_supplies_the_icon_its_density_holds() {
    let dir = scratch("split");
    let base = dir.join("base.apk");
    let split = dir.join("split.apk");
    Apk::new()
        .manifest(&manifest(Value::Ref(LABEL), Some(ICON)))
        .file("resources.arsc", &table().encode())
        .write(&base);
    Apk::new()
        .file(
            "resources.arsc",
            &Arsc::new()
                .string(
                    ICON,
                    Config::density(480),
                    "res/mipmap-xxhdpi/ic_launcher.png",
                )
                .encode(),
        )
        .file("res/mipmap-xxhdpi/ic_launcher.png", &png("xxhdpi"))
        .write(&split);
    let parsed = uxdroid_apk::inspect(&base).unwrap().manifest;

    let look = appearance(&parsed, &[&base, &split]);

    assert_eq!(look.icon.unwrap().bytes, png("xxhdpi"));
}

#[test]
fn an_app_without_a_table_has_only_a_literal_label() {
    let dir = scratch("no-table");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&manifest(Value::Ref(LABEL), Some(ICON)))
        .write(&apk);
    let parsed = uxdroid_apk::inspect(&apk).unwrap().manifest;

    let look = appearance(&parsed, &[&apk]);

    assert_eq!(look.label, None);
    assert_eq!(look.icon, None);
}

#[test]
fn truncation_at_any_length_never_panics() {
    let bytes = table().sparse().encode();
    for length in 0..bytes.len() {
        if let Some(table) = ResourceTable::parse(&bytes[..length]) {
            let _ = table.labels(LABEL);
            let _ = table.icon_files(ICON);
        }
    }
}

#[test]
fn corrupting_any_byte_never_panics() {
    for arsc in [table(), table().sparse()] {
        let bytes = arsc.encode();
        for position in 0..bytes.len() {
            for value in [0x00, 0x7F, 0x80, 0xFF] {
                let mut damaged = bytes.clone();
                damaged[position] = value;
                if let Some(table) = ResourceTable::parse(&damaged) {
                    let _ = table.labels(LABEL);
                    let _ = table.icon_files(ICON);
                }
            }
        }
    }
}

/// References that fan out to many configurations, each referring on, must not cost more than a
/// linear walk.
#[test]
fn fanning_references_resolve_in_bounded_time() {
    let mut arsc = Arsc::new();
    for id in 0..8u32 {
        for density in 1..=200u16 {
            arsc = arsc.reference(
                0x7f02_0000 + id,
                Config::density(density),
                0x7f02_0000 + id + 1,
            );
        }
    }
    let bytes = arsc.encode();
    let table = ResourceTable::parse(&bytes).unwrap();

    let start = Instant::now();
    let _ = table.icon_files(ICON);

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}

const ATTR_DRAWABLE: u32 = 0x0101_0199;
const FOREGROUND: u32 = 0x7f02_0001;
const BACKGROUND: u32 = 0x7f02_0002;

fn adaptive_icon() -> Vec<u8> {
    Axml::new()
        .start("adaptive-icon", &[])
        .start(
            "background",
            &[android("drawable", ATTR_DRAWABLE, Value::Ref(BACKGROUND))],
        )
        .end("background")
        .start(
            "foreground",
            &[android("drawable", ATTR_DRAWABLE, Value::Ref(FOREGROUND))],
        )
        .end("foreground")
        .end("adaptive-icon")
        .encode()
}

fn adaptive_app(path: &std::path::Path, icon_xml: &[u8]) {
    Apk::new()
        .manifest(&manifest(Value::Str("Adaptive"), Some(ICON)))
        .file(
            "resources.arsc",
            &Arsc::new()
                .string(
                    ICON,
                    Config::density(0xFFFE),
                    "res/mipmap-anydpi-v26/ic_launcher.xml",
                )
                .string(
                    FOREGROUND,
                    Config::density(480),
                    "res/mipmap-xxhdpi/ic_launcher_foreground.png",
                )
                .string(
                    BACKGROUND,
                    Config::density(480),
                    "res/mipmap-xxhdpi/background.png",
                )
                .encode(),
        )
        .file("res/mipmap-anydpi-v26/ic_launcher.xml", icon_xml)
        .file(
            "res/mipmap-xxhdpi/ic_launcher_foreground.png",
            &png("foreground"),
        )
        .file("res/mipmap-xxhdpi/background.png", &png("background"))
        .write(path);
}

#[test]
fn an_adaptive_only_icon_falls_back_to_its_foreground_layer() {
    let dir = scratch("adaptive");
    let apk = dir.join("app.apk");
    adaptive_app(&apk, &adaptive_icon());
    let parsed = uxdroid_apk::inspect(&apk).unwrap().manifest;

    let look = appearance(&parsed, &[&apk]);

    assert_eq!(look.icon.unwrap().bytes, png("foreground"));
}

#[test]
fn a_damaged_adaptive_icon_never_panics() {
    let dir = scratch("adaptive-damaged");
    let apk = dir.join("app.apk");
    let bytes = adaptive_icon();
    for length in 0..bytes.len() {
        adaptive_app(&apk, &bytes[..length]);
        let parsed = uxdroid_apk::inspect(&apk).unwrap().manifest;
        let _ = appearance(&parsed, &[&apk]);
    }
}
