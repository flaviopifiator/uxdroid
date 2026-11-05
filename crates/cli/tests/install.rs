//! `uxdroid install`: the native libraries of an app the framework launcher starts, its split APKs,
//! and the signature checks before any of it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use uxdroid_testkit::{
    ATTR_IS_SPLIT_REQUIRED, ATTR_REQUIRED_SPLIT_TYPES, ATTR_SPLIT_TYPES, ATTR_TARGET_SDK,
    ATTR_VERSION_CODE, Apk, Axml, BLOCK_V31, INSTALLED_DATA, Key, Past, Value, android, apks_table,
    dex, plain, sample_manifest, split_manifest,
};

/// The ABI this host runs apps with.
const ABI: &str = if cfg!(target_arch = "aarch64") {
    "arm64-v8a"
} else {
    "x86_64"
};

/// ART's name for this host's instruction set, which names the oat directory.
const ISA: &str = if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x86_64"
};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("install-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The built `uxdroid`, started through `UXDROID_TEST_RUNNER` when that is set, as the aarch64
/// build is under QEMU user.
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

fn uxdroid(args: &[&str]) -> Output {
    uxdroid_command().args(args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Installs with no runtime to be found, as these tests always did.
fn install(apk: &Path, data: &Path) -> Output {
    install_with(apk, data, &[("UXDROID_RUNTIME", "")])
}

fn install_with(apk: &Path, data: &Path, env: &[(&str, &str)]) -> Output {
    install_all(&[apk], data, env)
}

fn install_all(apks: &[&Path], data: &Path, env: &[(&str, &str)]) -> Output {
    let mut command = uxdroid_command();
    command
        .arg("install")
        .args(apks)
        .arg("--data")
        .arg(data)
        .env("XDG_DATA_HOME", "/nonexistent")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
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
        &["install", "a.apk", "--data", "/x", "b.apk"],
        &["install", "a.apk", "--other", "b.apk", "--data", "/x"],
    ] {
        let output = uxdroid(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&output.stderr),
            "usage: uxdroid install <apk>... --data <dir>\n       uxdroid install <package> [--from <source>] [--refresh] [--data <dir>]\n",
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
        .native_lib(ABI, "liba.so")
        .native_lib(ABI, "libb.so")
        .native_lib("arm64-v8a", "libarm.so")
        .file("lib/x86_64/notes.txt", b"not a library")
        .write_signed(&apk, &Key::test());
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let app = data.join("data/com.example.app");
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 2 native libraries for {ABI} in {}\n",
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
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libnew.so")
        .write_signed(&new, &Key::test());

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
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libnew.so")
        .write_signed(&new, &Key::test());
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
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());
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
        .write_signed(&apk, &Key::test());
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
        .write_signed(&apk, &Key::test());
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
        .write_signed(&apk, &Key::test());
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
        .write_signed(&apk, &Key::test());
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: cannot translate com.example.app: libnowhere.so imports what no library, table or the guest libc has: uxdroid_nowhere\n"
    );
    assert!(!data.join("data/com.example.app/lib").exists());
}

/// Installs an APK whose only native library is `name`, built for arm64 from `source`.
#[cfg(target_arch = "x86_64")]
fn install_arm64(test: &str, name: &str, source: &str, flags: &[&str]) -> (Output, PathBuf) {
    let dir = scratch(test);
    let path = dir.join("lib.cpp");
    std::fs::write(&path, source).unwrap();
    let library = arm64_library(&dir, name, &path, flags);
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file(&format!("lib/arm64-v8a/{name}"), &library)
        .write_signed(&apk, &Key::test());
    let data = dir.join("data");
    (install(&apk, &data), data)
}

#[cfg(target_arch = "x86_64")]
fn plan_imports(data: &Path, library: &str) -> serde_json::Value {
    let plan = data.join("data/com.example.app/lib/arm64-v8a.plan.json");
    let plan: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plan).unwrap()).unwrap();
    plan["libraries"][library]["imports"].clone()
}

/// LinkedIn's Cronet build imports `ATrace_beginSection` and `ATrace_endSection`, which uxdroid's
/// libandroid lacks.
#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_calling_ndk_functions_uxdroid_lacks_installs_with_a_warning() {
    let (output, data) = install_arm64(
        "arm64-atrace",
        "libcronet.so",
        "#include <android/trace.h>\n\
         extern \"C\" void traced(void) { ATrace_beginSection(\"x\"); ATrace_endSection(); }\n",
        &["-landroid"],
    );

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        stdout.contains(
            "warning: com.example.app: libcronet.so calls functions uxdroid does not have yet, \
             and stops if it reaches one: ATrace_beginSection, ATrace_endSection\n"
        ),
        "{stdout}"
    );
    let imports = plan_imports(&data, "libcronet.so");
    assert_eq!(
        imports["ATrace_beginSection"],
        "unimplemented: libandroid.so"
    );
    assert_eq!(imports["ATrace_endSection"], "unimplemented: libandroid.so");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_needing_an_ndk_library_uxdroid_lacks_installs_with_a_warning() {
    let (output, data) = install_arm64(
        "arm64-vulkan",
        "libvk.so",
        "#include <vulkan/vulkan.h>\n\
         extern \"C\" int make(void) {\n\
           VkInstanceCreateInfo info = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};\n\
           VkInstance instance;\n\
           return vkCreateInstance(&info, nullptr, &instance);\n\
         }\n",
        &["-lvulkan"],
    );

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        stdout.contains(
            "warning: com.example.app: libvk.so calls functions uxdroid does not have yet, \
             and stops if it reaches one: vkCreateInstance\n"
        ),
        "{stdout}"
    );
    assert_eq!(
        plan_imports(&data, "libvk.so")["vkCreateInstance"],
        "unimplemented: libvulkan.so"
    );
}

/// Data cannot stop the program when read, so an unresolved data import still fails.
#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_data_import_found_nowhere_still_fails_the_install() {
    let (output, data) = install_arm64(
        "arm64-data",
        "libdata.so",
        "extern \"C\" int uxdroid_nowhere_data;\n\
         extern \"C\" int reads_it(void) { return uxdroid_nowhere_data; }\n",
        &["-Wl,-z,undefs", "-landroid"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: cannot translate com.example.app: libdata.so imports what no library, table or the guest libc has: uxdroid_nowhere_data\n"
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
    Apk::new()
        .manifest(&manifest)
        .write_signed(&apk, &Key::test());
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
        .write_signed(path, &Key::test());
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
                builder = builder.native_lib(ABI, &format!("lib{name}{index}.so"));
            }
            builder.write_signed(&apk, &Key::test());
            apk
        })
        .collect();

    for _ in 0..ROUNDS {
        let children: Vec<_> = (0..INSTALLS)
            .map(|index| {
                uxdroid_command()
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
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());
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
        .native_lib(ABI, "liba.so")
        .write_signed(&apk, &Key::test());
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
            "installed com.example.app: 1 native libraries for {ABI} in {}\n\
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
        listing(&code.join("oat").join(ISA)),
        ["base.odex", "base.vdex"]
    );
    assert_eq!(listing(&data.join("data/com.example.app")), ["abi", "lib"]);
    assert_eq!(
        listing(&data.join("app")),
        [
            ".com.example.app.lock",
            ".com.example.app.signer",
            "com.example.app"
        ]
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
        format!("--instruction-set={ISA}"),
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
        .write_signed(&old, &Key::test());
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("assets/v", b"2")
        .write_signed(&new, &Key::test());

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
        [
            ".com.example.app.lock",
            ".com.example.app.signer",
            "com.example.app"
        ]
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
            .native_lib(ABI, "libold.so")
            .write_signed(&old, &Key::test());
        let new = dir.join("new.apk");
        Apk::new()
            .manifest(&sample_manifest())
            .native_lib(ABI, "libnew.so")
            .write_signed(&new, &Key::test());
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
            [
                ".com.example.app.lock",
                ".com.example.app.signer",
                "com.example.app"
            ]
        );
    }
}

#[test]
fn without_a_framework_the_app_is_not_prepared_and_an_old_preparation_goes() {
    let dir = scratch("unprepared");
    let data = dir.join("data");
    let (runtime, _) = fake_runtime(&dir);
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());
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
            "installed com.example.app: 0 native libraries for {ABI} in {}\n",
            data.join("data/com.example.app/lib").display()
        )
    );
    assert_eq!(
        text(&output.stderr),
        "note: no framework in /no/runtime, so com.example.app is not prepared: its manifest is parsed, \
         its default bodies made and its code run uncompiled at each start\n"
    );
    assert_eq!(
        listing(&data.join("app")),
        [".com.example.app.lock", ".com.example.app.signer"]
    );
}

#[test]
fn a_framework_that_cannot_be_looked_for_is_named_as_such() {
    let dir = scratch("no-home");
    let data = dir.join("data");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());
    let output = uxdroid_command()
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
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&apk, &Key::test());

    let output = install(&apk, Path::new("data"));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: the data directory must be absolute, not data\n"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn a_signed_single_apk_installs_as_before_and_its_signer_is_recorded() {
    let dir = scratch("single");
    let apk = dir.join("app.apk");
    let key = Key::test();
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "liba.so")
        .write_signed(&apk, &key);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 1 native libraries for {ABI} in {}\n",
            data.join("data/com.example.app/lib").display()
        )
    );
    assert_eq!(
        text(&output.stderr),
        "note: no framework in /nonexistent/uxdroid/runtime, so com.example.app is not prepared: \
         its manifest is parsed, its default bodies made and its code run uncompiled at each start\n"
    );
    assert_eq!(listing(&data.join("data/com.example.app")), ["abi", "lib"]);
    assert_eq!(
        std::fs::read_to_string(data.join("app/.com.example.app.signer")).unwrap(),
        format!("{}\n", key.fingerprint())
    );
}

/// As a package installs it: `uxdroid` and its runtime directory side by side.
#[cfg(target_arch = "x86_64")]
#[test]
fn a_runtime_beside_the_program_is_used_when_the_user_has_none() {
    let dir = scratch("beside");
    let program = dir.join("lib/uxdroid/uxdroid");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_uxdroid"), &program).unwrap();
    let runtime = dir.join("lib/uxdroid/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "liba.so")
        .write_signed(&apk, &Key::test());
    let data = dir.join("data");

    let output = Command::new(&program)
        .arg("install")
        .arg(&apk)
        .arg("--data")
        .arg(&data)
        .env("XDG_DATA_HOME", dir.join("share"))
        .env_remove("UXDROID_RUNTIME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stderr),
        format!(
            "note: no framework in {}, so com.example.app is not prepared: its manifest is parsed, \
             its default bodies made and its code run uncompiled at each start\n",
            runtime.display()
        )
    );
}

/// The user's own runtime wins over one beside the program.
#[cfg(target_arch = "x86_64")]
#[test]
fn the_user_s_runtime_wins_over_one_beside_the_program() {
    let dir = scratch("user-runtime");
    let program = dir.join("lib/uxdroid/uxdroid");
    std::fs::create_dir_all(dir.join("lib/uxdroid/runtime")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_uxdroid"), &program).unwrap();
    let user = dir.join("share/uxdroid/runtime");
    std::fs::create_dir_all(&user).unwrap();
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "liba.so")
        .write_signed(&apk, &Key::test());

    let output = Command::new(&program)
        .arg("install")
        .arg(&apk)
        .arg("--data")
        .arg(dir.join("data"))
        .env("XDG_DATA_HOME", dir.join("share"))
        .env_remove("UXDROID_RUNTIME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).starts_with(&format!("note: no framework in {}, ", user.display())),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn an_unsigned_apk_is_refused() {
    let dir = scratch("unsigned");
    let apk = dir.join("app.apk");
    Apk::new().manifest(&sample_manifest()).write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: {}: it is not signed, and Android installs only signed APKs\n",
            apk.display()
        )
    );
    assert!(!data.exists());
}

#[test]
fn an_apk_changed_after_signing_is_refused() {
    let dir = scratch("tampered");
    let apk = dir.join("app.apk");
    let mut bytes = Apk::new()
        .manifest(&sample_manifest())
        .signed_bytes(&Key::test());
    bytes[60] ^= 1;
    std::fs::write(&apk, bytes).unwrap();
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: {}: its signature does not verify: its contents do not match their signed \
             digest, so it was changed after signing\n",
            apk.display()
        )
    );
    assert!(!data.exists());
}

/// `sample_manifest` with an older target SDK.
fn manifest_targeting(target_sdk: u32) -> Axml {
    Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str("com.example.app")),
                android("versionCode", ATTR_VERSION_CODE, Value::Int(42)),
            ],
        )
        .start(
            "uses-sdk",
            &[android(
                "targetSdkVersion",
                ATTR_TARGET_SDK,
                Value::Int(target_sdk),
            )],
        )
        .end("uses-sdk")
        .end("manifest")
}

#[test]
fn an_apk_with_only_a_v1_signature_installs() {
    let dir = scratch("v1");
    let apk = dir.join("app.apk");
    let key = Key::other();
    Apk::new()
        .manifest(&manifest_targeting(29))
        .with_v1_signature(&key, None)
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        std::fs::read_to_string(data.join("app/.com.example.app.signer")).unwrap(),
        format!("{}\n", key.fingerprint())
    );
}

#[test]
fn a_v1_signature_alone_is_refused_for_an_apk_targeting_sdk_30_or_later() {
    let dir = scratch("v1-target");
    let apk = dir.join("app.apk");
    Apk::new()
        .manifest(&manifest_targeting(30))
        .with_v1_signature(&Key::test(), None)
        .write(&apk);
    let data = dir.join("data");

    let output = install(&apk, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: {}: it targets SDK 30, which needs a v2 or later signature, and it has only v1\n",
            apk.display()
        )
    );
    assert!(!data.exists());
}

#[test]
fn an_update_signed_by_another_certificate_is_refused_and_the_installed_app_stays() {
    let dir = scratch("other-signer");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    let new = dir.join("new.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libnew.so")
        .write_signed(&new, &Key::other());
    assert_eq!(install(&old, &data).status.code(), Some(0));

    let output = install(&new, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: com.example.app is installed signed by a different certificate, so this \
             update is refused: installed {}, update {}\n",
            Key::test().fingerprint(),
            Key::other().fingerprint()
        )
    );
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libold.so"]
    );
    assert_eq!(
        std::fs::read_to_string(data.join("app/.com.example.app.signer")).unwrap(),
        format!("{}\n", Key::test().fingerprint())
    );
}

/// The base and config splits a bundletool build of `com.example.app` delivers, by file name.
fn split_set(key: &Key) -> Vec<(&'static str, Vec<u8>)> {
    let split = |name: &'static str| {
        Apk::new().manifest(&split_manifest("com.example.app", 42, name, None, false))
    };
    vec![
        (
            "base-master.apk",
            Apk::new()
                .manifest(&sample_manifest())
                .file("classes.dex", &dex(1, 1))
                .signed_bytes(key),
        ),
        (
            "base-x86_64.apk",
            split("config.x86_64")
                .native_lib("x86_64", "libsplit.so")
                .signed_bytes(key),
        ),
        (
            "base-arm64_v8a.apk",
            split("config.arm64_v8a")
                .native_lib("arm64-v8a", "libsplit.so")
                .signed_bytes(key),
        ),
        ("base-mdpi.apk", split("config.mdpi").signed_bytes(key)),
        ("base-xxhdpi.apk", split("config.xxhdpi").signed_bytes(key)),
        ("base-en.apk", split("config.en").signed_bytes(key)),
        ("base-fr.apk", split("config.fr").signed_bytes(key)),
    ]
}

fn write_files(dir: &Path, files: &[(&str, Vec<u8>)]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|(name, bytes)| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect()
}

/// The ABI split of `split_set` for this host, and the file it is in.
const HOST_SPLIT: (&str, &str) = if cfg!(target_arch = "aarch64") {
    ("config.arm64_v8a", "base-arm64_v8a.apk")
} else {
    ("config.x86_64", "base-x86_64.apk")
};

const CHOSEN: &str = if cfg!(target_arch = "aarch64") {
    "installed splits of com.example.app: config.arm64_v8a (abi), config.en (language), \
     config.mdpi (density); skipped config.fr (language), config.x86_64 (abi), config.xxhdpi \
     (density)\n"
} else {
    "installed splits of com.example.app: config.en (language), config.mdpi (density), \
     config.x86_64 (abi); skipped config.arm64_v8a (abi), config.fr (language), config.xxhdpi \
     (density)\n"
};

/// What an install of `split_set` leaves, without a framework.
fn assert_split_install(output: &Output, data: &Path, files: &[(&str, Vec<u8>)]) {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "installed com.example.app: 1 native libraries for {ABI} in {}\n{CHOSEN}",
            data.join("data/com.example.app/lib").display()
        )
    );
    let code = data.join("app/com.example.app");
    let (abi_split, abi_file) = HOST_SPLIT;
    let mut expected = [
        "base.apk".to_string(),
        "split_config.en.apk".to_string(),
        "split_config.mdpi.apk".to_string(),
        format!("split_{abi_split}.apk"),
        "splits".to_string(),
    ];
    expected.sort();
    assert_eq!(listing(&code), expected);
    let bytes = |name: &str| {
        files
            .iter()
            .find(|(file, _)| *file == name)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(
        std::fs::read(code.join("base.apk")).unwrap(),
        bytes("base-master.apk")
    );
    assert_eq!(
        std::fs::read(code.join(format!("split_{abi_split}.apk"))).unwrap(),
        bytes(abi_file)
    );
    let mut names = ["config.en", "config.mdpi", abi_split];
    names.sort();
    assert_eq!(
        std::fs::read_to_string(code.join("splits")).unwrap(),
        format!("{}\n", names.join("\n"))
    );
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libsplit.so"]
    );
}

#[test]
fn a_base_and_its_config_splits_install_the_ones_for_this_machine() {
    let dir = scratch("splits");
    let files = split_set(&Key::test());
    let paths = write_files(&dir, &files);
    let data = dir.join("data");
    let apks: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();

    let output = install_all(&apks, &data, &[("UXDROID_RUNTIME", "")]);

    assert_split_install(&output, &data, &files);
}

#[test]
fn a_bundletool_apks_installs_the_variant_for_this_sdk() {
    let dir = scratch("apks");
    let files = split_set(&Key::test());
    let paths: Vec<String> = files
        .iter()
        .map(|(name, _)| format!("splits/{name}"))
        .collect();
    // A variant for a future SDK holds other APKs under the same split names.
    let future: Vec<String> = paths
        .iter()
        .map(|path| path.replace(".apk", "_2.apk"))
        .collect();
    let mut bundle = Apk::new().file(
        "toc.pb",
        &apks_table(
            "com.example.app",
            &[(21, paths.clone()), (99, future.clone())],
        ),
    );
    for (path, (_, bytes)) in paths.iter().zip(&files) {
        bundle = bundle.file(path, bytes);
    }
    for path in &future {
        bundle = bundle.file(
            path,
            &Apk::new()
                .manifest(&sample_manifest())
                .signed_bytes(&Key::other()),
        );
    }
    let apks = dir.join("app.apks");
    bundle.write(&apks);
    let data = dir.join("data");

    let output = install_with(&apks, &data, &[("UXDROID_RUNTIME", "")]);

    assert_split_install(&output, &data, &files);
}

#[test]
fn concurrent_bundle_installs_do_not_remove_each_others_unpacked_apks() {
    const INSTALLS: usize = 8;
    const ROUNDS: usize = 20;
    let dir = scratch("concurrent-bundles");
    let files = split_set(&Key::test());
    let paths: Vec<String> = files
        .iter()
        .map(|(name, _)| format!("splits/{name}"))
        .collect();
    let mut bundle = Apk::new().file(
        "toc.pb",
        &apks_table("com.example.app", &[(21, paths.clone())]),
    );
    for (path, (_, bytes)) in paths.iter().zip(&files) {
        bundle = bundle.file(path, bytes);
    }
    let apks = dir.join("app.apks");
    bundle.write(&apks);
    let data = dir.join("data");

    for round in 0..ROUNDS {
        let children: Vec<_> = (0..INSTALLS)
            .map(|_| {
                uxdroid_command()
                    .args(["install", apks.to_str().unwrap(), "--data"])
                    .arg(&data)
                    .env("UXDROID_RUNTIME", "")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "round {round}: {}",
                text(&output.stderr)
            );
        }
    }
    let left: Vec<String> = listing(&data.join("app"))
        .into_iter()
        .filter(|name| name.starts_with(".unpack"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn an_xapk_installs_the_apks_it_holds() {
    let dir = scratch("xapk");
    let files = split_set(&Key::test());
    let mut bundle = Apk::new()
        .file("manifest.json", br#"{"package_name": "com.example.app"}"#)
        .file("icon.png", b"\x89PNG");
    for (name, bytes) in &files {
        bundle = bundle.file(name, bytes);
    }
    let xapk = dir.join("app.xapk");
    bundle.write(&xapk);
    let data = dir.join("data");

    let output = install_with(&xapk, &data, &[("UXDROID_RUNTIME", "")]);

    assert_split_install(&output, &data, &files);
    // Nothing of the unpacked bundle is left.
    assert_eq!(
        listing(&data.join("app")),
        [
            ".com.example.app.lock",
            ".com.example.app.signer",
            "com.example.app"
        ]
    );
}

/// Installs `split_set` with `replace` swapping one file, expecting `message` and no install.
fn refused_split_set(name: &str, replace: (&'static str, Vec<u8>), message: &str) {
    let dir = scratch(name);
    let mut files = split_set(&Key::test());
    match files.iter_mut().find(|(file, _)| *file == replace.0) {
        Some(file) => file.1 = replace.1,
        None => files.push(replace),
    }
    let paths = write_files(&dir, &files);
    let data = dir.join("data");
    let apks: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();

    let output = install_all(&apks, &data, &[("UXDROID_RUNTIME", "")]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout));
    assert_eq!(text(&output.stderr), format!("error: {message}\n"));
    assert!(!data.join("app/com.example.app").exists());
    assert!(!data.join("data").exists());
}

#[test]
fn a_split_of_another_package_is_refused() {
    refused_split_set(
        "split-package",
        (
            "base-en.apk",
            Apk::new()
                .manifest(&split_manifest(
                    "com.example.other",
                    42,
                    "config.en",
                    None,
                    false,
                ))
                .signed_bytes(&Key::test()),
        ),
        "split config.en is for package com.example.other, not com.example.app",
    );
}

#[test]
fn a_split_of_another_version_is_refused() {
    refused_split_set(
        "split-version",
        (
            "base-en.apk",
            Apk::new()
                .manifest(&split_manifest(
                    "com.example.app",
                    43,
                    "config.en",
                    None,
                    false,
                ))
                .signed_bytes(&Key::test()),
        ),
        "split config.en has versionCode 43, but the base APK has 42",
    );
}

#[test]
fn a_split_given_twice_is_refused() {
    refused_split_set(
        "split-twice",
        (
            "base-en-again.apk",
            Apk::new()
                .manifest(&split_manifest(
                    "com.example.app",
                    42,
                    "config.en",
                    None,
                    false,
                ))
                .file("assets/again", b"1")
                .signed_bytes(&Key::test()),
        ),
        "split config.en is given twice",
    );
}

#[test]
fn a_split_signed_by_another_certificate_is_refused() {
    refused_split_set(
        "split-signer",
        (
            "base-en.apk",
            Apk::new()
                .manifest(&split_manifest(
                    "com.example.app",
                    42,
                    "config.en",
                    None,
                    false,
                ))
                .signed_bytes(&Key::other()),
        ),
        "split config.en is signed by a different certificate than the base APK",
    );
}

#[test]
fn two_base_apks_are_refused() {
    let dir = scratch("two-bases");
    let one = dir.join("one.apk");
    let two = dir.join("two.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .write_signed(&one, &Key::test());
    Apk::new()
        .manifest(&sample_manifest())
        .file("assets/x", b"x")
        .write_signed(&two, &Key::test());
    let data = dir.join("data");

    let output = install_all(&[&one, &two], &data, &[("UXDROID_RUNTIME", "")]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: {} and {} are both base APKs; give one app's base with its splits\n",
            one.display(),
            two.display()
        )
    );
}

#[test]
fn splits_without_their_base_are_refused() {
    let dir = scratch("no-base");
    let split = dir.join("split.apk");
    Apk::new()
        .manifest(&split_manifest(
            "com.example.app",
            42,
            "config.en",
            None,
            false,
        ))
        .write_signed(&split, &Key::test());

    let output = install(&split, &dir.join("data"));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: none of the APKs is a base APK; give the base with its splits\n"
    );
}

/// A base APK and one ABI split, `config`, holding `library` as `lib/<abi>/libspike.so`.
#[cfg(target_arch = "x86_64")]
fn base_with_abi_split(
    dir: &Path,
    config: &'static str,
    abi: &str,
    library: &[u8],
) -> Vec<PathBuf> {
    let key = Key::test();
    let files = vec![
        (
            "base-master.apk",
            Apk::new()
                .manifest(&sample_manifest())
                .file("classes.dex", &dex(1, 1))
                .signed_bytes(&key),
        ),
        (
            "base-abi.apk",
            Apk::new()
                .manifest(&split_manifest("com.example.app", 42, config, None, false))
                .file(&format!("lib/{abi}/libspike.so"), library)
                .signed_bytes(&key),
        ),
    ];
    write_files(dir, &files)
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_abi_split_only_for_arm64_is_translated_on_x86_64() {
    let dir = scratch("split-arm64");
    let library = arm64_library(&dir, "libspike.so", &spike_source(), &["-llog"]);
    let paths = base_with_abi_split(&dir, "config.arm64_v8a", "arm64-v8a", &library);
    let apks: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let data = dir.join("data");

    let output = install_all(&apks, &data, &[("UXDROID_RUNTIME", "")]);

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
    assert!(stdout.contains("translated for x86_64: "), "{stdout}");
    assert!(
        stdout.ends_with("installed splits of com.example.app: config.arm64_v8a (abi)\n"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(data.join("data/com.example.app/abi")).unwrap(),
        "arm64-v8a\n"
    );
    assert_eq!(listing(&libraries), ["arm64-v8a.plan.json", "libspike.so"]);
    assert_eq!(
        listing(&data.join("app/com.example.app")),
        ["base.apk", "split_config.arm64_v8a.apk", "splits"]
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn abi_splits_only_for_a_cpu_with_no_translator_say_translation_is_missing() {
    let dir = scratch("split-foreign");
    let paths = base_with_abi_split(&dir, "config.armeabi_v7a", "armeabi-v7a", b"not a library");
    let apks: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let data = dir.join("data");

    let output = install_all(&apks, &data, &[("UXDROID_RUNTIME", "")]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: this app has native code only for armeabi-v7a, and running it on x86_64 needs \
         translation, which is not built yet\n"
    );
    assert!(!data.join("data/com.example.app").exists());
}

#[test]
fn a_base_that_requires_its_splits_is_refused_alone() {
    let dir = scratch("split-required");
    let apk = dir.join("base.apk");
    let manifest = Axml::new()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.app"))],
        )
        .start(
            "application",
            &[android(
                "isSplitRequired",
                ATTR_IS_SPLIT_REQUIRED,
                Value::Int(0xffff_ffff),
            )],
        )
        .end("application")
        .end("manifest");
    Apk::new()
        .manifest(&manifest)
        .write_signed(&apk, &Key::test());

    let output = install(&apk, &dir.join("data"));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: com.example.app is delivered as split APKs and needs them: give the base APK with \
         its splits, or the bundle\n"
    );
}

#[test]
fn a_required_split_type_that_no_chosen_split_provides_is_refused() {
    let dir = scratch("split-types");
    let base = dir.join("base.apk");
    let manifest = Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str("com.example.app")),
                android(
                    "requiredSplitTypes",
                    ATTR_REQUIRED_SPLIT_TYPES,
                    Value::Str("base__density"),
                ),
            ],
        )
        .end("manifest");
    Apk::new()
        .manifest(&manifest)
        .write_signed(&base, &Key::test());
    let split = dir.join("split.apk");
    let split_manifest = Axml::new()
        .start(
            "manifest",
            &[
                plain("package", Value::Str("com.example.app")),
                plain("split", Value::Str("config.en")),
                android("splitTypes", ATTR_SPLIT_TYPES, Value::Str("base__language")),
            ],
        )
        .end("manifest");
    Apk::new()
        .manifest(&split_manifest)
        .write_signed(&split, &Key::test());

    let output = install_all(
        &[&base, &split],
        &dir.join("data"),
        &[("UXDROID_RUNTIME", "")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: com.example.app needs a split of type base__density, which none of the chosen \
         splits provides\n"
    );
}

#[test]
fn with_a_framework_feature_split_code_is_compiled_after_the_base() {
    let dir = scratch("feature");
    let key = Key::test();
    let base = dir.join("base.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .file("classes.dex", &dex(1, 1))
        .write_signed(&base, &key);
    let feature = dir.join("feature.apk");
    Apk::new()
        .manifest(&split_manifest("com.example.app", 42, "chat", None, true))
        .file("classes.dex", &dex(1, 1))
        .write_signed(&feature, &key);
    let feature_config = dir.join("feature-fr.apk");
    Apk::new()
        .manifest(&split_manifest(
            "com.example.app",
            42,
            "chat.config.fr",
            Some("chat"),
            false,
        ))
        .write_signed(&feature_config, &key);
    let data = dir.join("data");
    let (runtime, log) = fake_runtime(&dir);

    let output = install_all(
        &[&base, &feature, &feature_config],
        &data,
        &[("UXDROID_RUNTIME", runtime.to_str().unwrap())],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains(
            "installed splits of com.example.app: chat (feature); skipped chat.config.fr (language)\n"
        ),
        "{}",
        text(&output.stdout)
    );
    let code = data.join("app/com.example.app");
    assert_eq!(
        listing(&code),
        [
            "base.apk",
            "defaults.so",
            "oat",
            "package.record",
            "split_chat.apk",
            "splits"
        ]
    );
    assert_eq!(
        listing(&code.join("oat").join(ISA)),
        [
            "base.odex",
            "base.vdex",
            "split_chat.odex",
            "split_chat.vdex"
        ]
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    let split = calls
        .lines()
        .find(|line| line.contains("--dex-location=") && line.contains("split_chat.apk"))
        .unwrap();
    // As installd does: the context names the APKs before it relative to the directory dex2oat reads
    // them from, since the final ones do not exist yet, and ART matches them by name at load.
    let staged = data.join("app/.com.example.app.new-");
    let context = format!(
        "--class-loader-context=PCL[base.apk];PCL[{0}/framework/framework.jar:{0}/framework/uxdroid-system.jar]",
        runtime.display()
    );
    assert!(
        split.split(' ').any(|word| word == context),
        "{context} in {split}"
    );
    assert!(
        split
            .split(' ')
            .any(|word| word.starts_with(&format!("--classpath-dir={}", staged.display()))),
        "--classpath-dir in {split}"
    );
}

/// An update signed by `new`, which rotated from `old` with `flags`.
fn rotated_update(dir: &Path, old: &Key, new: &Key, flags: u32) -> PathBuf {
    let path = dir.join("rotated.apk");
    let past = [Past {
        key: old,
        flags,
        forged_by: None,
        algorithm: None,
    }];
    let bytes = Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libnew.so")
        .signed_rotated(&past, new, BLOCK_V31);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn an_update_by_a_rotated_key_installs_when_its_lineage_trusts_the_installed_signer() {
    let dir = scratch("rotated-signer");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    assert_eq!(install(&old, &data).status.code(), Some(0));

    let new = rotated_update(&dir, &Key::test(), &Key::other(), INSTALLED_DATA);
    let output = install(&new, &data);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libnew.so"]
    );
    // From now on the app is the rotated key's.
    assert_eq!(
        std::fs::read_to_string(data.join("app/.com.example.app.signer")).unwrap(),
        format!("{}\n", Key::other().fingerprint())
    );
}

#[test]
fn an_update_signed_by_the_installed_key_and_another_is_refused() {
    let dir = scratch("added-signer");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    assert_eq!(install(&old, &data).status.code(), Some(0));
    let signer = data.join("app/.com.example.app.signer");
    let before = std::fs::read_to_string(&signer).unwrap();

    // Android compares signer sets exactly: a second signer makes another app.
    let new = dir.join("two.apk");
    let bytes = Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libnew.so")
        .signed_by_all(&[&Key::test(), &Key::other()]);
    std::fs::write(&new, bytes).unwrap();
    let output = install(&new, &data);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("is installed signed by a different certificate"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&signer).unwrap(), before);
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libold.so"]
    );
}

#[test]
fn an_update_by_a_rotated_key_is_refused_when_its_lineage_withholds_the_installed_data() {
    let dir = scratch("rotated-no-data");
    let data = dir.join("data");
    let old = dir.join("old.apk");
    Apk::new()
        .manifest(&sample_manifest())
        .native_lib(ABI, "libold.so")
        .write_signed(&old, &Key::test());
    assert_eq!(install(&old, &data).status.code(), Some(0));

    let output = install(&rotated_update(&dir, &Key::test(), &Key::other(), 0), &data);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("is installed signed by a different certificate"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        listing(&data.join("data/com.example.app/lib")),
        ["libold.so"]
    );
}
