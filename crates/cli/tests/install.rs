//! `uxdroid install`: the native libraries of an app the framework launcher starts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use uxdroid_testkit::{Apk, Axml, Value, plain, sample_manifest};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("install-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn uxdroid(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn install(apk: &Path, data: &Path) -> Output {
    uxdroid(&[
        "install",
        apk.to_str().unwrap(),
        "--data",
        data.to_str().unwrap(),
    ])
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn wrong_usage_exits_2() {
    for args in [
        &["install"][..],
        &["install", "a.apk"],
        &["install", "a.apk", "--data"],
        &["install", "a.apk", "--other", "/x"],
        &["install", "--data", "/x"],
        &["install", "a.apk", "--data", "/x", "extra"],
    ] {
        let output = uxdroid(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&output.stderr),
            "usage: uxdroid install <apk> --data <dir>\n",
            "{args:?}"
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn host_libraries_go_to_the_native_library_directory_with_the_abi_recorded() {
    let dir = scratch("host");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "liba.so")
        .native_lib("x86_64", "libb.so")
        .native_lib("arm64-v8a", "libarm.so")
        .file("lib/x86_64/notes.txt", b"not a library")
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let app = data.join("data/com.example.app");
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 2 native libraries for x86_64 in {}\n",
            app.join("lib").display()
        )
    );
    assert_eq!(listing(&app.join("lib")), ["liba.so", "libb.so"]);
    assert_eq!(std::fs::read(app.join("lib/liba.so")).unwrap(), b"\x7fELF");
    assert_eq!(
        std::fs::read_to_string(app.join("abi")).unwrap(),
        "x86_64\n"
    );
    // Nothing is left from staging.
    assert_eq!(listing(&app), ["abi", "lib"]);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn installing_again_replaces_the_old_libraries() {
    let dir = scratch("again");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "libold.so")
        .write(&old);
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "libnew.so")
        .write(&new);

    assert_eq!(install(&old, &data).status.code(), Some(0));
    assert_eq!(install(&new, &data).status.code(), Some(0));

    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libnew.so"]
    );
}

#[test]
fn an_install_that_fails_keeps_the_previous_libraries() {
    let dir = scratch("keeps");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "libold.so")
        .write(&old);
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "libnew.so")
        .write(&new);
    assert_eq!(install(&old, &data).status.code(), Some(0));
    // The ABI record cannot be written over a directory, so the second install fails late.
    let record = data.join("data/com.example.app/abi");
    std::fs::remove_file(&record).unwrap();
    std::fs::create_dir(&record).unwrap();

    let output = install(&new, &data);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libold.so"]
    );
    assert_eq!(listing(&data.join("data/com.example.app")), ["abi", "lib"]);
}

#[test]
fn an_apk_without_native_code_installs_an_empty_directory() {
    let dir = scratch("none");
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains(": 0 native libraries for "));
    assert!(listing(&data.join("data/com.example.app/lib")).is_empty());
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_apk_only_for_another_cpu_says_translation_is_missing() {
    let dir = scratch("foreign");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("arm64-v8a", "liba.so")
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("only for arm64-v8a"),
        "{}",
        text(&output.stderr)
    );
    assert!(!data.exists());
}

#[test]
fn a_package_name_cannot_choose_the_directory() {
    let dir = scratch("package");
    let apk = dir.join("app.apk");
    let manifest = Axml::new()
        .start("manifest", &[plain("package", Value::Str("../../escape"))])
        .start("application", &[])
        .end("application")
        .end("manifest");
    Apk::new().manifest(&manifest).write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).starts_with("error: bad package name"),
        "{}",
        text(&output.stderr)
    );
    assert!(!data.exists());
}

/// Writes an APK whose archive names one library twice, as a crafted file can: the second entry is
/// written under a name of the same length, which is then renamed in place.
fn write_duplicate_entries(path: &Path) {
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/x86_64/liba.so", b"\x7fELF first")
        .file("lib/x86_64/libz.so", b"\x7fELF second")
        .write(path);
    let mut bytes = std::fs::read(path).unwrap();
    let (from, to) = (b"lib/x86_64/libz.so", b"lib/x86_64/liba.so");
    let mut renamed = 0;
    for start in 0..bytes.len() - from.len() {
        if &bytes[start..start + from.len()] == from {
            bytes[start..start + from.len()].copy_from_slice(to);
            renamed += 1;
        }
    }
    // Once in its local header and once in the central directory.
    assert_eq!(renamed, 2);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn an_apk_naming_an_entry_twice_is_refused() {
    let dir = scratch("duplicate");
    let apk = dir.join("app.apk");
    write_duplicate_entries(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout));
    assert_eq!(
        text(&output.stderr),
        "error: not a valid APK archive: it names an entry twice, which Android refuses\n"
    );
    assert!(!data.join("data/com.example.app/lib").exists());
}

#[cfg(target_arch = "x86_64")]
#[test]
fn concurrent_installs_of_one_package_all_succeed_and_leave_one_install() {
    const INSTALLS: usize = 12;
    const ROUNDS: usize = 3;
    let dir = scratch("concurrent");
    let data = dir.join("data");
    let apks: Vec<PathBuf> = ["one", "two"]
        .iter()
        .map(|name| {
            let apk = dir.join(format!("{name}.apk"));
            let mut builder = Apk::new().manifest(&sample_manifest());
            for index in 0..20 {
                builder = builder.native_lib("x86_64", &format!("lib{name}{index}.so"));
            }
            builder.write(&apk);
            apk
        })
        .collect();

    for _ in 0..ROUNDS {
        let children: Vec<_> = (0..INSTALLS)
            .map(|index| {
                Command::new(env!("CARGO_BIN_EXE_uxdroid"))
                    .args(["install", apks[index % 2].to_str().unwrap(), "--data"])
                    .arg(&data)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        }
        let app = data.join("data/com.example.app");
        assert_eq!(listing(&app), ["abi", "lib"]);
        let libraries = listing(&app.join("lib"));
        assert_eq!(libraries.len(), 20, "{libraries:?}");
        assert!(
            libraries.iter().all(|name| name.starts_with("libone"))
                || libraries.iter().all(|name| name.starts_with("libtwo")),
            "{libraries:?}"
        );
    }
}

#[test]
fn an_install_removes_what_an_interrupted_one_left() {
    let dir = scratch("leftovers");
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);
    let data = dir.join("data");
    let app = data.join("data/com.example.app");
    for leftover in [".lib-1", ".old-lib-1"] {
        std::fs::create_dir_all(app.join(leftover)).unwrap();
        std::fs::write(app.join(leftover).join("liba.so"), b"\x7fELF").unwrap();
    }
    std::fs::write(app.join(".abi-1"), "x86_64\n").unwrap();

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(listing(&app), ["abi", "lib"]);
}

#[test]
fn a_relative_data_directory_is_refused() {
    let dir = scratch("relative");
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);

    let output = install(&apk, Path::new("data"));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: the data directory must be absolute, not data\n"
    );
}
