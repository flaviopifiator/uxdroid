//! Installs into the default data directory add the app to the desktop's application menu: a
//! `.desktop` entry with its label and icon, which `uxdroid remove` deletes again.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use uxdroid_testkit::{
    ATTR_ICON, ATTR_LABEL, ATTR_NAME, ATTR_VERSION_CODE, Apk, Arsc, Axml, Config, Key, Value,
    android, plain, png, sample_manifest,
};

const PACKAGE: &str = "com.example.app";
const LABEL: u32 = 0x7f01_0000;
const ICON: u32 = 0x7f02_0000;

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("desktop-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn uxdroid_command() -> Command {
    match std::env::var_os("UXDROID_TEST_RUNNER") {
        Some(runner) => {
            let mut command = Command::new(runner);
            command.arg(env!("CARGO_BIN_EXE_uxdroid"));
            command
        }
        None => Command::new(env!("CARGO_BIN_EXE_uxdroid")),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A home whose `XDG_DATA_HOME` holds the default data directory, `uxdroid/data`.
struct Home {
    share: PathBuf,
}

impl Home {
    fn new(name: &str) -> Self {
        let share = scratch(name).join("share");
        std::fs::create_dir_all(&share).unwrap();
        Home { share }
    }

    fn data(&self) -> PathBuf {
        self.share.join("uxdroid/data")
    }

    fn entry(&self) -> PathBuf {
        self.share
            .join(format!("applications/uxdroid-{PACKAGE}.desktop"))
    }

    fn icon(&self) -> PathBuf {
        self.share.join(format!("uxdroid/icons/{PACKAGE}.png"))
    }

    fn run(&self, args: &[&str], data: &Path) -> Output {
        uxdroid_command()
            .args(args)
            .arg("--data")
            .arg(data)
            .env("XDG_DATA_HOME", &self.share)
            .env("UXDROID_RUNTIME", "")
            .env("LANG", "en_US.UTF-8")
            .env_remove("LANGUAGE")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .output()
            .unwrap()
    }

    fn install(&self, apk: &Path) -> Output {
        self.run(&["install", apk.to_str().unwrap()], &self.data())
    }
}

fn ok(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

/// An app whose application is labelled `label` and has the launcher icon `ICON`, launched by
/// `.MainActivity`.
fn labelled(label: Value, version: u32) -> Axml {
    Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str(PACKAGE)),
                android("versionCode", ATTR_VERSION_CODE, Value::Int(version)),
            ],
        )
        .start(
            "application",
            &[
                android("label", ATTR_LABEL, label),
                android("icon", ATTR_ICON, Value::Ref(ICON)),
            ],
        )
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

fn resources() -> Arsc {
    Arsc::new()
        .string(LABEL, Config::DEFAULT, "Example")
        .string(LABEL, Config::locale("es", ""), "Ejemplo")
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
            Config::density(0xFFFE),
            "res/mipmap-anydpi-v26/ic_launcher.xml",
        )
}

fn app(dir: &Path, manifest: &Axml) -> PathBuf {
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(manifest)
        .file("resources.arsc", &resources().encode())
        .file("res/mipmap-mdpi/ic_launcher.png", &png("mdpi"))
        .file("res/mipmap-xxhdpi/ic_launcher.png", &png("xxhdpi"))
        .file("res/mipmap-anydpi-v26/ic_launcher.xml", b"\x03\x00\x08\x00")
        .write_signed(&apk, &Key::test());
    apk
}

fn exe() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_uxdroid"))
        .unwrap()
        .display()
        .to_string()
}

#[test]
fn an_install_into_the_default_data_directory_adds_the_app_to_the_menu() {
    let home = Home::new("added");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Ref(LABEL), 1),
    );

    let output = home.install(&apk);

    // What install prints is unchanged.
    assert_eq!(
        ok(&output),
        format!(
            "installed {PACKAGE}: 0 native libraries for {} in {}\n",
            uxdroid_apk::host_abi(),
            home.data().join(format!("data/{PACKAGE}/lib")).display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(home.entry()).unwrap(),
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Example\n\
             Name[es]=Ejemplo\n\
             Exec={} app {PACKAGE} start\n\
             Icon={}\n\
             StartupWMClass=uxdroid-{PACKAGE}\n\
             Terminal=false\n\
             X-Uxdroid-Package={PACKAGE}\n",
            exe(),
            home.icon().display()
        )
    );
    // The sharpest PNG, not the adaptive icon's XML.
    assert_eq!(std::fs::read(home.icon()).unwrap(), png("xxhdpi"));
}

#[test]
fn remove_deletes_the_entry_and_its_icon_and_prints_what_it_did() {
    let home = Home::new("removed");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Ref(LABEL), 1),
    );
    ok(&home.install(&apk));

    let output = home.run(&["remove", PACKAGE], &home.data());

    assert_eq!(
        ok(&output),
        format!("removed {PACKAGE}: its code, data, signer record and component settings\n")
    );
    assert!(!home.entry().exists());
    assert!(!home.icon().exists());
    assert_eq!(
        std::fs::read_dir(home.share.join("applications"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn an_app_without_a_launcher_activity_gets_no_entry() {
    let home = Home::new("no-launcher");
    let manifest = Axml::new()
        .start("manifest", &[plain("package", Value::Str(PACKAGE))])
        .start(
            "application",
            &[android("label", ATTR_LABEL, Value::Str("Hidden"))],
        )
        .end("application")
        .end("manifest");
    let apk = app(home.share.parent().unwrap(), &manifest);

    ok(&home.install(&apk));

    assert!(!home.entry().exists());
    assert!(!home.share.join("applications").exists());
}

#[test]
fn hostile_label_characters_cannot_add_keys_or_field_codes() {
    let home = Home::new("hostile");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Str("Evil\nExec=sh -c boom\r\\x %f"), 1),
    );

    ok(&home.install(&apk));

    let entry = std::fs::read_to_string(home.entry()).unwrap();
    assert!(
        entry.contains("\nName=Evil Exec=sh -c boom \\\\x %f\n"),
        "{entry}"
    );
    assert_eq!(
        entry
            .lines()
            .filter(|line| line.starts_with("Exec="))
            .count(),
        1,
        "{entry}"
    );
}

#[test]
fn an_app_with_no_label_or_icon_is_named_after_its_package() {
    let home = Home::new("bare");
    let apk = home.share.parent().unwrap().join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());

    ok(&home.install(&apk));

    let entry = std::fs::read_to_string(home.entry()).unwrap();
    assert!(entry.contains(&format!("\nName={PACKAGE}\n")), "{entry}");
    assert!(!entry.contains("Icon="), "{entry}");
    assert!(!home.icon().exists());
}

#[test]
fn an_install_into_another_data_directory_leaves_the_menu_alone() {
    let home = Home::new("other-data");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Ref(LABEL), 1),
    );
    let other = home.share.parent().unwrap().join("elsewhere");

    ok(&home.run(&["install", apk.to_str().unwrap()], &other));

    assert!(!home.entry().exists());
    assert!(!home.icon().exists());
}

#[test]
fn an_entry_uxdroid_did_not_write_is_left_as_it_is() {
    let home = Home::new("foreign");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Ref(LABEL), 1),
    );
    std::fs::create_dir_all(home.entry().parent().unwrap()).unwrap();
    std::fs::write(home.entry(), "[Desktop Entry]\nName=Mine\n").unwrap();

    let output = home.install(&apk);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        text(&output.stderr).contains("was not written by uxdroid; it is left as it is"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(home.entry()).unwrap(),
        "[Desktop Entry]\nName=Mine\n"
    );
}

#[test]
fn a_symlink_at_the_entry_is_never_written_through() {
    let home = Home::new("symlink");
    let apk = app(
        home.share.parent().unwrap(),
        &labelled(Value::Ref(LABEL), 1),
    );
    let target = home.share.parent().unwrap().join("target");
    std::fs::write(&target, "untouched").unwrap();
    std::fs::create_dir_all(home.entry().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, home.entry()).unwrap();

    let output = home.install(&apk);
    assert_eq!(output.status.code(), Some(0));
    ok(&home.run(&["remove", PACKAGE], &home.data()));

    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    assert!(
        std::fs::symlink_metadata(home.entry())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn an_update_rewrites_the_entry_and_an_update_without_a_launcher_removes_it() {
    let home = Home::new("update");
    let dir = home.share.parent().unwrap();
    ok(&home.install(&app(dir, &labelled(Value::Ref(LABEL), 1))));
    ok(&home.install(&app(dir, &labelled(Value::Str("Renamed"), 2))));
    let renamed = std::fs::read_to_string(home.entry()).unwrap();

    let headless = Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str(PACKAGE)),
                android("versionCode", ATTR_VERSION_CODE, Value::Int(3)),
            ],
        )
        .start("application", &[])
        .end("application")
        .end("manifest");
    ok(&home.install(&app(dir, &headless)));

    assert!(renamed.contains("\nName=Renamed\n"), "{renamed}");
    assert!(!home.entry().exists());
    assert!(!home.icon().exists());
}
