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

/// Installs with no runtime to be found, as these tests always did.
fn install(apk: &Path, data: &Path) -> Output {
    install_with(apk, data, &[("UXDROID_RUNTIME", "")])
}

fn install_with(apk: &Path, data: &Path, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uxdroid"));
    command
        .args(["install", apk.to_str().unwrap(), "--data"])
        .arg(data)
        .env("XDG_DATA_HOME", "/nonexistent")
        .env_remove("FAKE_FAIL");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

/// A runtime whose dalvikvm64 and dex2oat64 are scripts: each logs its arguments, writes what the real
/// one writes, and fails when FAKE_FAIL names it.
fn fake_runtime(dir: &Path) -> (PathBuf, PathBuf) {
    let runtime = dir.join("runtime");
    let log = dir.join("calls.log");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    std::fs::create_dir_all(runtime.join("framework")).unwrap();
    for file in ["framework.jar", "uxdroid-system.jar", "natives.txt"] {
        std::fs::write(runtime.join("framework").join(file), b"").unwrap();
    }
    let scripts = [
        (
            "dalvikvm64",
            "[ \"$FAKE_FAIL\" = installer ] && { echo 'Exception in thread main: bad manifest' >&2; exit 1; }\n\
             eval staged=\\${$(($# - 1))}\n\
             dir=$(dirname \"$staged\")\n\
             echo record > \"$dir/package.record\"\n\
             echo library > \"$dir/defaults.so\"\n\
             echo 'launcher com.example.app.MainActivity'\n\
             echo 'defaults 3'\n",
        ),
        (
            "dex2oat64",
            "[ \"$FAKE_FAIL\" = dex2oat ] && { echo '11-03 E dex2oat64: dex file is broken' >&2; echo 'dex2oat took 1ms' >&2; exit 1; }\n\
             for arg; do case $arg in --oat-file=*) out=${arg#--oat-file=};; esac; done\n\
             echo odex > \"$out\"\n\
             echo vdex > \"${out%.odex}.vdex\"\n",
        ),
    ];
    for (name, body) in scripts {
        let path = runtime.join("bin").join(name);
        let script = format!(
            "#!/bin/sh\necho \"{name} ANDROID_DATA=$ANDROID_DATA $*\" >> {}\n{body}",
            log.display()
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (runtime, log)
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
fn an_apk_only_for_a_cpu_with_no_translator_says_translation_is_missing() {
    let dir = scratch("foreign");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("armeabi-v7a", "liba.so")
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: this APK has native code only for armeabi-v7a, and running it on x86_64 needs translation, which is not built yet\n"
    );
    assert!(!data.exists());
}

/// Builds an arm64 library with the NDK, which the dev image has.
#[cfg(target_arch = "x86_64")]
fn arm64_library(dir: &Path, name: &str, source: &Path, extra: &[&str]) -> Vec<u8> {
    let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run in the dev image");
    let clang = Path::new(&ndk)
        .join("toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang++");
    let out = dir.join(name);
    let status = Command::new(clang)
        .args(["-O2", "-fPIC", "-shared", "-static-libstdc++"])
        .arg(source)
        .args(extra)
        .arg("-o")
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(out).unwrap()
}

#[cfg(target_arch = "x86_64")]
fn spike_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridge/tests/fixtures/arm64/spike.cpp")
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_only_apk_installs_for_translation_with_every_import_resolved() {
    let dir = scratch("arm64");
    let library = arm64_library(&dir, "libspike.so", &spike_source(), &["-llog"]);
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/arm64-v8a/libspike.so", &library)
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let libraries = data.join("data/com.example.app/lib");
    assert!(
        stdout.starts_with(&format!(
            "installed com.example.app: 1 native libraries for arm64-v8a in {}\n",
            libraries.display()
        )),
        "{stdout}"
    );
    let plan_path = libraries.join("arm64-v8a.plan.json");
    assert!(
        stdout.contains(&format!("plan in {}", plan_path.display())),
        "{stdout}"
    );
    assert!(stdout.contains("translated for x86_64: "), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(data.join("data/com.example.app/abi")).unwrap(),
        "arm64-v8a\n"
    );
    assert_eq!(listing(&libraries), ["arm64-v8a.plan.json", "libspike.so"]);

    let plan: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plan_path).unwrap()).unwrap();
    let spike = &plan["libraries"]["libspike.so"];
    assert_eq!(plan["abi"], "arm64-v8a");
    assert!(
        spike["needed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "liblog.so")
    );
    let jni: Vec<&str> = spike["jni"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        jni.contains(&"JNI_OnLoad") && jni.contains(&"Java_dev_uxdroid_Spike_scale"),
        "{jni:?}"
    );
    assert!(!spike["functions"].as_array().unwrap().is_empty());
    let imports = &spike["imports"];
    assert_eq!(imports["qsort"], "libc");
    assert_eq!(imports["stat"], "libc");
    assert_eq!(imports["__android_log_print"], "table liblog.so");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_needing_a_library_nobody_provides_fails_the_install_naming_it() {
    let dir = scratch("arm64-needed");
    let source = dir.join("lib.cpp");
    std::fs::write(&source, "extern \"C\" int nothing(void) { return 0; }\n").unwrap();
    arm64_library(&dir, "libnotthere.so", &source, &[]);
    let library = arm64_library(
        &dir,
        "libneeds.so",
        &source,
        &[
            "-Wl,--no-as-needed",
            "-L",
            dir.to_str().unwrap(),
            "-lnotthere",
        ],
    );
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/arm64-v8a/libneeds.so", &library)
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: cannot translate com.example.app: libneeds.so needs libnotthere.so, which is neither in the APK nor a system library uxdroid provides\n"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_import_found_nowhere_fails_the_install_naming_it() {
    let dir = scratch("arm64-unresolved");
    let source = dir.join("lib.cpp");
    std::fs::write(
        &source,
        "extern \"C\" int uxdroid_nowhere(void);\n\
         extern \"C\" int calls_it(void) { return uxdroid_nowhere(); }\n",
    )
    .unwrap();
    let library = arm64_library(&dir, "libnowhere.so", &source, &["-Wl,-z,undefs"]);
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("lib/arm64-v8a/libnowhere.so", &library)
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: cannot translate com.example.app: libnowhere.so imports what no library, table or the guest libc has: uxdroid_nowhere\n"
    );
    assert!(!data.join("data/com.example.app/lib").exists());
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
fn with_a_framework_the_code_is_compiled_and_the_manifest_and_default_bodies_recorded() {
    let dir = scratch("prepared");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib("x86_64", "liba.so")
        .write(&apk);
    let data = dir.join("data");
    let (runtime, log) = fake_runtime(&dir);

    let output = install_with(
        &apk,
        &data,
        &[("UXDROID_RUNTIME", runtime.to_str().unwrap())],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let code = data.join("app/com.example.app");
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 1 native libraries for x86_64 in {}\n\
             prepared com.example.app in {}: code compiled (verify), manifest recorded with launcher \
             com.example.app.MainActivity, 3 default bodies\n",
            data.join("data/com.example.app/lib").display(),
            code.display()
        )
    );
    assert_eq!(
        std::fs::read(code.join("base.apk")).unwrap(),
        std::fs::read(&apk).unwrap()
    );
    assert_eq!(
        listing(&code),
        ["base.apk", "defaults.so", "oat", "package.record"]
    );
    assert_eq!(
        listing(&code.join("oat/x86_64")),
        ["base.odex", "base.vdex"]
    );
    assert_eq!(listing(&data.join("data/com.example.app")), ["abi", "lib"]);
    assert_eq!(
        listing(&data.join("app")),
        [".com.example.app.lock", "com.example.app"]
    );

    let calls = std::fs::read_to_string(&log).unwrap();
    let installer = calls
        .lines()
        .find(|line| line.starts_with("dalvikvm64 "))
        .unwrap();
    assert!(
        installer.contains(&format!("ANDROID_DATA={} ", data.display())),
        "{installer}"
    );
    // The installer reads the staged copy and records the path it will have once installed.
    let staged = data.join("app/.com.example.app.new-");
    assert!(
        installer.contains(&format!(
            " dev.uxdroid.system.Installer {} {} {}",
            runtime.display(),
            Path::new(env!("CARGO_BIN_EXE_uxdroid"))
                .with_file_name("libuxdroid_system.so")
                .display(),
            staged.display()
        )) && installer.ends_with(&format!("/base.apk {}", code.join("base.apk").display())),
        "{installer}"
    );
    let dex2oat = calls
        .lines()
        .find(|line| line.starts_with("dex2oat64 "))
        .unwrap();
    for argument in [
        "--compiler-filter=verify".to_string(),
        "--instruction-set=x86_64".to_string(),
        format!("--dex-location={}", code.join("base.apk").display()),
        format!(
            "--class-loader-context=PCL[];PCL[{0}/framework/framework.jar:{0}/framework/uxdroid-system.jar]",
            runtime.display()
        ),
    ] {
        assert!(
            dex2oat.split(' ').any(|word| word == argument),
            "{argument} in {dex2oat}"
        );
    }
}

#[test]
fn installing_again_with_a_framework_replaces_the_prepared_app() {
    let dir = scratch("prepared-again");
    let data = dir.join("data");
    let (runtime, _) = fake_runtime(&dir);
    let env = [("UXDROID_RUNTIME", runtime.to_str().unwrap())];
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("assets/v", b"1")
        .write(&old);
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("assets/v", b"2")
        .write(&new);

    assert_eq!(install_with(&old, &data, &env).status.code(), Some(0));
    let output = install_with(&new, &data, &env);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let code = data.join("app/com.example.app");
    assert_eq!(
        std::fs::read(code.join("base.apk")).unwrap(),
        std::fs::read(&new).unwrap()
    );
    assert_eq!(
        listing(&code),
        ["base.apk", "defaults.so", "oat", "package.record"]
    );
    assert_eq!(
        listing(&data.join("app")),
        [".com.example.app.lock", "com.example.app"]
    );
}

#[test]
fn a_preparation_that_fails_fails_the_install_and_keeps_the_previous_one() {
    for (failing, message) in [
        (
            "dex2oat",
            "dex2oat failed: 11-03 E dex2oat64: dex file is broken",
        ),
        (
            "installer",
            "the framework installer failed: Exception in thread main: bad manifest",
        ),
    ] {
        let dir = scratch(&format!("prepare-fails-{failing}"));
        let data = dir.join("data");
        let (runtime, _) = fake_runtime(&dir);
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
        let env = [("UXDROID_RUNTIME", runtime.to_str().unwrap())];
        assert_eq!(install_with(&old, &data, &env).status.code(), Some(0));

        let output = install_with(&new, &data, &[env[0], ("FAKE_FAIL", failing)]);

        assert_eq!(output.status.code(), Some(1), "{failing}");
        assert_eq!(
            text(&output.stderr),
            format!("error: cannot prepare com.example.app for the framework: {message}\n")
        );
        let code = data.join("app/com.example.app");
        assert_eq!(
            std::fs::read(code.join("base.apk")).unwrap(),
            std::fs::read(&old).unwrap()
        );
        assert_eq!(
            listing(&data.join("data/com.example.app/lib")),
            ["libold.so"]
        );
        assert_eq!(listing(&data.join("data/com.example.app")), ["abi", "lib"]);
        assert_eq!(
            listing(&data.join("app")),
            [".com.example.app.lock", "com.example.app"]
        );
    }
}

#[test]
fn without_a_framework_the_app_is_not_prepared_and_an_old_preparation_goes() {
    let dir = scratch("unprepared");
    let data = dir.join("data");
    let (runtime, _) = fake_runtime(&dir);
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);
    assert_eq!(
        install_with(
            &apk,
            &data,
            &[("UXDROID_RUNTIME", runtime.to_str().unwrap())]
        )
        .status
        .code(),
        Some(0)
    );

    let output = install_with(&apk, &data, &[("UXDROID_RUNTIME", "/no/runtime")]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 0 native libraries for x86_64 in {}\n",
            data.join("data/com.example.app/lib").display()
        )
    );
    assert_eq!(
        text(&output.stderr),
        "note: no framework in /no/runtime, so com.example.app is not prepared: its manifest is parsed, \
         its default bodies made and its code run uncompiled at each start\n"
    );
    assert_eq!(listing(&data.join("app")), [".com.example.app.lock"]);
}

#[test]
fn a_framework_that_cannot_be_looked_for_is_named_as_such() {
    let dir = scratch("no-home");
    let data = dir.join("data");
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);
    let output = Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(["install", apk.to_str().unwrap(), "--data"])
        .arg(&data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "")
        .env_remove("HOME")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stderr),
        "note: cannot look for the framework (HOME is not set), so com.example.app is not prepared: \
         its manifest is parsed, its default bodies made and its code run uncompiled at each start\n"
    );
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
