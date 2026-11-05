//! `uxdroid run` failures that need no runtime. Running an app is covered by app_smoke.sh.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::PathBuf;
use std::process::{Command, Output};

use uxdroid_testkit::{ATTR_NAME, Apk, Axml, Value, android, plain, sample_manifest};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("run-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn uxdroid(args: &[&str], home: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("UXDROID_RUNTIME")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn wrong_usage_exits_2() {
    let home = scratch("usage");
    for args in [
        &["run"][..],
        &["run", "a.apk", "--unknown"],
        &["run", "a.apk", "--runtime"],
        &["run", "a.apk", "--runtime", "x", "--runtime", "y"],
        &["run", "a.apk", "b.apk"],
        &["run", "--exit-after-first-frame"],
    ] {
        let output = uxdroid(args, &home);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).starts_with("usage: uxdroid run"),
            "{args:?}"
        );
    }
}

#[test]
fn an_unreadable_apk_is_an_error() {
    let home = scratch("unreadable");
    let output = uxdroid(&["run", "/does/not/exist.apk"], &home);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).starts_with("error: cannot read APK"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_apk_without_a_launcher_is_an_error() {
    let home = scratch("no-launcher");
    let apk = home.join("app.apk");
    let manifest = Axml::new()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.none"))],
        )
        .start("application", &[])
        .start(
            "activity",
            &[android("name", ATTR_NAME, Value::Str(".Plain"))],
        )
        .end("activity")
        .end("application")
        .end("manifest");
    Apk::new().manifest(&manifest).write(&apk);

    let output = uxdroid(&["run", apk.to_str().unwrap()], &home);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: this APK has no launcher Activity\n"
    );
}

#[test]
fn an_apk_only_for_another_cpu_says_translation_is_missing() {
    let home = scratch("foreign");
    let apk = home.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("arm64-v8a", "liba.so")
        .write(&apk);

    let output = uxdroid(&["run", apk.to_str().unwrap()], &home);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("only for arm64-v8a"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_package_name_cannot_choose_the_cache_directory() {
    let home = scratch("package");
    let apk = home.join("app.apk");
    let manifest = Axml::new()
        .start("manifest", &[plain("package", Value::Str("../../escape"))])
        .start("application", &[])
        .start(
            "activity",
            &[android("name", ATTR_NAME, Value::Str(".Main"))],
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
        .end("manifest");
    Apk::new().manifest(&manifest).write(&apk);

    let output = uxdroid(&["run", apk.to_str().unwrap()], &home);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("bad package name"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_missing_runtime_is_named() {
    let home = scratch("no-runtime");
    let apk = home.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);

    let output = uxdroid(
        &["run", apk.to_str().unwrap(), "--runtime", "/no/runtime"],
        &home,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot find the runtime at /no/runtime"),
        "{}",
        stderr(&output)
    );
}
