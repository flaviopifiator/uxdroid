use std::path::PathBuf;
use std::process::{Command, Output};

use uxdroid_apk::host_abi;
use uxdroid_testkit::{
    ATTR_NAME, ATTR_TARGET_ACTIVITY, ATTR_VERSION_NAME, Apk, Axml, Value, android, dex, plain,
    sample_manifest,
};

const FOREIGN_ABI: &str = "riscv64";

fn apk_path(test: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{test}.apk"))
}

fn uxdroid(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(args)
        .output()
        .unwrap()
}

fn inspect(test: &str, apk: &Apk) -> Output {
    let path = apk_path(test);
    apk.write(&path);
    uxdroid(&["inspect", path.to_str().unwrap()])
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn line<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(key))
        .unwrap_or_else(|| panic!("no {key} in {text}"))
}

#[test]
fn prints_the_full_report() {
    let host = host_abi();
    let apk = Apk::new()
        .manifest(&sample_manifest())
        .file("classes.dex", &dex(100, 800))
        .file("classes2.dex", &dex(50, 100))
        .file("assets/classes3.dex", &dex(999, 999))
        .native_lib(FOREIGN_ABI, "liba.so")
        .native_lib(FOREIGN_ABI, "libb.so")
        .native_lib(host, "liba.so");

    let output = inspect("full_report", &apk);

    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    let mut abis = [format!("{FOREIGN_ABI} (2)"), format!("{host} (1)")];
    abis.sort();
    assert_eq!(
        stdout(&output),
        format!(
            "package: com.example.app\n\
             version: 1.2.3 (42)\n\
             sdk: min 21, target 34\n\
             launcher: com.example.app.MainActivity\n\
             dex: 2 files, 150 classes, 900 methods\n\
             abis: {}\n\
             execution: direct\n",
            abis.join(", ")
        )
    );
}

#[test]
fn only_foreign_libraries_need_the_abi_bridge() {
    let apk = Apk::new()
        .manifest(&sample_manifest())
        .native_lib(FOREIGN_ABI, "liba.so");

    let output = inspect("foreign_only", &apk);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        line(&stdout(&output), "execution:"),
        format!("execution: needs-abi-bridge ({FOREIGN_ABI})")
    );
}

#[test]
fn no_native_libraries_run_direct() {
    let apk = Apk::new()
        .manifest(&sample_manifest())
        .file("classes.dex", &dex(1, 1));

    let text = stdout(&inspect("no_native", &apk));

    assert_eq!(line(&text, "abis:"), "abis: none");
    assert_eq!(line(&text, "execution:"), "execution: direct");
}

#[test]
fn reads_utf16_string_pools() {
    let manifest = Axml::new()
        .utf16()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.utf16"))],
        )
        .end("manifest");

    let text = stdout(&inspect("utf16", &Apk::new().manifest(&manifest)));

    assert_eq!(line(&text, "package:"), "package: com.example.utf16");
}

#[test]
fn identifies_attributes_by_resource_id_when_names_are_stripped() {
    let text = stdout(&inspect(
        "stripped",
        &Apk::new().manifest(&sample_manifest().strip_attr_names()),
    ));

    assert_eq!(line(&text, "version:"), "version: 1.2.3 (42)");
    assert_eq!(line(&text, "sdk:"), "sdk: min 21, target 34");
    assert_eq!(
        line(&text, "launcher:"),
        "launcher: com.example.app.MainActivity"
    );
}

#[test]
fn launcher_alias_resolves_to_its_target_activity() {
    let manifest = Axml::new()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.app"))],
        )
        .start("application", &[])
        .start(
            "activity-alias",
            &[
                android("name", ATTR_NAME, Value::Str(".Alias")),
                android(
                    "targetActivity",
                    ATTR_TARGET_ACTIVITY,
                    Value::Str("org.other.Real"),
                ),
            ],
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
        .end("activity-alias")
        .end("application")
        .end("manifest");

    let text = stdout(&inspect("alias", &Apk::new().manifest(&manifest)));

    assert_eq!(line(&text, "launcher:"), "launcher: org.other.Real");
}

#[test]
fn reports_missing_and_referenced_values() {
    let manifest = Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str("com.example.app")),
                android("versionName", ATTR_VERSION_NAME, Value::Ref(0x7f0e_0001)),
            ],
        )
        .end("manifest");

    let text = stdout(&inspect("missing", &Apk::new().manifest(&manifest)));

    assert_eq!(line(&text, "version:"), "version: @0x7f0e0001 (unknown)");
    assert_eq!(line(&text, "sdk:"), "sdk: min unknown, target unknown");
    assert_eq!(line(&text, "launcher:"), "launcher: none");
    assert_eq!(line(&text, "dex:"), "dex: 0 files, 0 classes, 0 methods");
}

#[test]
fn file_that_is_not_a_zip_is_an_error() {
    let path = apk_path("not_zip");
    std::fs::write(&path, b"plain text").unwrap();

    let output = uxdroid(&["inspect", path.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with("error: not a valid APK archive"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn missing_file_is_an_error() {
    let output = uxdroid(&["inspect", "/nonexistent/app.apk"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).starts_with("error: cannot read APK"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn apk_without_manifest_is_an_error() {
    let output = inspect("no_manifest", &Apk::new().file("classes.dex", &dex(1, 1)));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "error: APK has no AndroidManifest.xml\n");
}

#[test]
fn manifest_without_package_is_an_error() {
    let manifest = Axml::new().start("manifest", &[]).end("manifest");

    let output = inspect("no_package", &Apk::new().manifest(&manifest));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: malformed manifest: no package name\n"
    );
}

#[test]
fn invalid_dex_is_an_error() {
    let apk = Apk::new()
        .manifest(&sample_manifest())
        .file("classes.dex", b"not dex");

    let output = inspect("bad_dex", &apk);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "error: malformed DEX classes.dex\n");
}

#[test]
fn wrong_usage_exits_2() {
    for args in [
        &[][..],
        &["inspect"],
        &["run", "app.apk"],
        &["inspect", "a.apk", "b.apk"],
    ] {
        let output = uxdroid(args);

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&output),
            "usage: uxdroid inspect <apk>\n",
            "{args:?}"
        );
    }
}
