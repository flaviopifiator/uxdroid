//! `uxdroid load` against libraries really built with the Android NDK.
//!
//! The NDK ships in the dev image. Without it these tests fail instead of skipping silently.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const MIN_SDK: &str = "23";
const CHECKS: &[&str] = &[
    "test_constructor",
    "test_dependency",
    "test_init_order",
    "test_weak_undefined",
    "test_data_relocations",
    "test_bss",
    "test_errno",
    "test_stdio",
    "test_fortify",
    "test_log",
    "test_signal_sets",
    "test_sigaction",
    "test_sigmask",
    "test_setjmp",
    "test_sysconf",
    "test_api_level",
    "test_threads",
    "test_semaphore",
    "test_dlopen",
    "test_getaddrinfo",
    "test_strerror_r",
    "test_ifunc",
    "test_emulated_tls",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn toolchain() -> PathBuf {
    let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run tests in the dev image");
    Path::new(&ndk).join("toolchains/llvm/prebuilt/linux-x86_64")
}

/// Builds `sources` into `<dir>/<output>` with the NDK's clang.
fn ndk_build(dir: &Path, output: &str, sdk: &str, sources: &[&str], flags: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let is_cxx = sources.iter().any(|source| source.ends_with(".cpp"));
    let clang = format!(
        "bin/x86_64-linux-android{sdk}-clang{}",
        if is_cxx { "++" } else { "" }
    );
    let status = Command::new(toolchain().join(clang))
        .args(sources.iter().map(|source| fixture(source)))
        .args(["-shared", "-fPIC", "-O1", "-D_FORTIFY_SOURCE=2"])
        .args(["-L", dir.to_str().unwrap()])
        .args(flags)
        .arg("-o")
        .arg(dir.join(output))
        .status()
        .unwrap();
    assert!(status.success(), "failed to build {output}");
}

fn build_root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("bionic-fixtures")
}

/// The variants differ in how the linker encodes what the loader must handle.
fn variants() -> &'static [(&'static str, PathBuf)] {
    static BUILT: OnceLock<Vec<(&'static str, PathBuf)>> = OnceLock::new();
    BUILT.get_or_init(|| {
        let variants: &[(&str, &[&str])] = &[
            ("plain", &["-Wl,--pack-dyn-relocs=none"]),
            ("packed", &["-Wl,--pack-dyn-relocs=android"]),
            (
                "relr",
                &["-Wl,--pack-dyn-relocs=relr", "-Wl,--use-android-relr-tags"],
            ),
            ("sysv-hash", &["-Wl,--hash-style=sysv"]),
            ("page-16k", &["-Wl,-z,max-page-size=16384"]),
        ];
        variants
            .iter()
            .map(|(name, flags)| {
                let dir = build_root().join(name);
                ndk_build(&dir, "libdep.so", MIN_SDK, &["dep.c"], &[]);
                let mut all_flags = vec!["-ldep", "-llog", "-ldl"];
                all_flags.extend_from_slice(flags);
                ndk_build(&dir, "libbasic.so", MIN_SDK, &["basic.c"], &all_flags);
                (*name, dir)
            })
            .collect()
    })
}

fn plain_dir() -> &'static Path {
    &variants()[0].1
}

fn uxdroid(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(args)
        .output()
        .unwrap()
}

fn load_and_call(library: &Path, symbol: &str) -> Output {
    uxdroid(&["load", library.to_str().unwrap(), "--call", symbol])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_check_passes(library: &Path, check: &str) {
    let output = load_and_call(library, check);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{check} on {}: {}",
        library.display(),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains(&format!("call {check}: 0\n")),
        "{check} on {}: {}\n{}",
        library.display(),
        stdout(&output),
        stderr(&output)
    );
}

#[test]
fn prints_the_loaded_library_and_the_call_result() {
    let library = plain_dir().join("libbasic.so");

    let output = load_and_call(&library, "answer");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "loaded: libbasic.so\nneeds: libdep.so, liblog.so, libdl.so, libc.so, libm.so\ncall answer: 42\n"
            .replace("libdl.so, libc.so, libm.so", needed_tail(&library).as_str())
    );
    assert_eq!(stderr(&output), "");
}

/// The order of implicit libraries belongs to the NDK, not to this test.
fn needed_tail(library: &Path) -> String {
    let readelf = Command::new(toolchain().join("bin/llvm-readelf"))
        .args(["-d", library.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&readelf.stdout).into_owned();
    let needed: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("NEEDED"))
        .filter_map(|line| line.split('[').nth(1)?.split(']').next())
        .filter(|name| !["libdep.so", "liblog.so"].contains(name))
        .collect();
    needed.join(", ")
}

#[test]
fn loading_without_a_call_runs_only_the_initializers() {
    let output = uxdroid(&["load", plain_dir().join("libbasic.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("loaded: libbasic.so\n"));
    assert!(!stdout(&output).contains("call "));
}

#[test]
fn every_bionic_check_passes_in_every_encoding() {
    for (_, dir) in variants() {
        for check in CHECKS {
            assert_check_passes(&dir.join("libbasic.so"), check);
        }
    }
}

#[test]
fn c_stdio_output_reaches_stdout_and_stderr() {
    let output = load_and_call(&plain_dir().join("libbasic.so"), "test_stdio");

    assert!(
        stdout(&output).contains("printed 7\n"),
        "{}",
        stdout(&output)
    );
    assert_eq!(stderr(&output), "to stderr\n");
}

#[test]
fn android_log_formats_register_float_and_stack_arguments() {
    let output = load_and_call(&plain_dir().join("libbasic.so"), "test_log");

    assert_eq!(
        stderr(&output),
        "I/fixture: plain message\nW/fixture: 1 two 3.5 4 5 6 7 8 9.5\n"
    );
}

#[test]
fn the_ndk_cxx_runtime_works_shared_and_static() {
    let dir = build_root().join("cxx");
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = toolchain().join("sysroot/usr/lib/x86_64-linux-android/libc++_shared.so");
    std::fs::copy(runtime, dir.join("libc++_shared.so")).unwrap();
    ndk_build(&dir, "libshared.so", MIN_SDK, &["cxx.cpp"], &[]);
    ndk_build(
        &dir,
        "libstatic.so",
        MIN_SDK,
        &["cxx.cpp"],
        &["-static-libstdc++"],
    );

    for library in ["libshared.so", "libstatic.so"] {
        for check in [
            "test_static_constructor",
            "test_exception",
            "test_containers",
            "test_std_thread",
        ] {
            assert_check_passes(&dir.join(library), check);
        }
    }
}

#[test]
fn streams_from_old_api_levels_reach_the_host_streams() {
    let dir = build_root().join("legacy");
    ndk_build(
        &dir,
        "liblegacy.so",
        MIN_SDK,
        &["legacy_stdio.c"],
        &["-Wl,-z,undefs"],
    );

    let output = load_and_call(&dir.join("liblegacy.so"), "test_legacy_streams");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "loaded: liblegacy.so\nneeds: {}\nlegacy fputs\nlegacy fwrite\ncall test_legacy_streams: 0\n",
            needed_tail(&dir.join("liblegacy.so"))
        )
    );
    assert_eq!(stderr(&output), "legacy 1 2.5 3 4 5 6 7 8\n");
}

#[test]
fn abi_incompatible_symbols_load_and_trap_by_name_when_called() {
    let output = load_and_call(&plain_dir().join("libbasic.so"), "trap_long_double");

    assert_eq!(output.status.code(), None, "must die from abort");
    assert!(
        stderr(&output).contains("uxdroid: strtold has an incompatible ABI"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn all_unresolved_symbols_are_listed() {
    let dir = build_root().join("unresolved");
    let source = dir.join("unresolved.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &source,
        "int missing_one(void); int missing_two(void);\n\
         int entry(void) { return missing_one() + missing_two(); }\n",
    )
    .unwrap();
    ndk_build(
        &dir,
        "libunresolved.so",
        MIN_SDK,
        &[source.to_str().unwrap()],
        &["-Wl,-z,undefs"],
    );

    let output = uxdroid(&["load", dir.join("libunresolved.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libunresolved.so: unresolved symbols: missing_one, missing_two\n"
    );
    assert_eq!(stdout(&output), "");
}

#[test]
fn missing_dependencies_are_named_and_search_dirs_find_them() {
    let dir = build_root().join("search");
    let deps = dir.join("deps");
    ndk_build(&deps, "libdep.so", MIN_SDK, &["dep.c"], &[]);
    let needs_dep = format!("-L{}", deps.display());
    ndk_build(
        &dir,
        "libbasic.so",
        MIN_SDK,
        &["basic.c"],
        &[&needs_dep, "-ldep", "-llog", "-ldl"],
    );
    let library = dir.join("libbasic.so");

    let without = uxdroid(&["load", library.to_str().unwrap()]);
    assert_eq!(without.status.code(), Some(1));
    assert_eq!(
        stderr(&without),
        "error: libbasic.so needs libdep.so, which was not found\n"
    );

    let with = uxdroid(&[
        "load",
        library.to_str().unwrap(),
        "--search-dir",
        deps.to_str().unwrap(),
        "--call",
        "test_dependency",
    ]);
    assert_eq!(with.status.code(), Some(0), "{}", stderr(&with));
    assert!(stdout(&with).ends_with("call test_dependency: 0\n"));
}

#[test]
fn elf_tls_is_rejected_by_name() {
    let dir = build_root().join("tls");
    let source = dir.join("tls.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &source,
        "__thread int value = 5;\nint entry(void) { return value; }\n",
    )
    .unwrap();
    ndk_build(&dir, "libtls.so", "29", &[source.to_str().unwrap()], &[]);

    let output = uxdroid(&["load", dir.join("libtls.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libtls.so: unsupported ELF: ELF TLS\n"
    );
}

#[test]
fn a_missing_symbol_to_call_is_an_error() {
    let output = load_and_call(&plain_dir().join("libbasic.so"), "no_such_function");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libbasic.so has no symbol no_such_function\n"
    );
}

#[test]
fn wrong_usage_exits_2() {
    for args in [
        &["load"][..],
        &["load", "a.so", "--call"],
        &["load", "a.so", "--search-dir"],
        &["load", "a.so", "--unknown"],
        &["load", "a.so", "b.so"],
    ] {
        let output = uxdroid(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("usage: uxdroid load"), "{args:?}");
    }
}

#[test]
fn inspect_still_reports_wrong_usage_the_same_way() {
    let output = uxdroid(&["inspect"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("usage: uxdroid inspect <apk>"));
}

#[test]
fn unreadable_and_non_elf_files_are_errors() {
    let dir = build_root().join("bad");
    std::fs::create_dir_all(&dir).unwrap();
    let text = dir.join("text.so");
    std::fs::write(
        &text,
        b"not an elf file, but long enough to hold an ELF header if it was one....",
    )
    .unwrap();

    let not_elf = uxdroid(&["load", text.to_str().unwrap()]);
    assert_eq!(not_elf.status.code(), Some(1));
    assert_eq!(
        stderr(&not_elf),
        "error: text.so: malformed ELF: bad magic\n"
    );

    let missing = uxdroid(&["load", dir.join("absent.so").to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).starts_with("error: cannot read"));

    let directory = uxdroid(&["load", dir.to_str().unwrap()]);
    assert_eq!(directory.status.code(), Some(1));
    assert!(stderr(&directory).contains("not a regular file"));
}

#[test]
fn truncated_libraries_fail_with_an_error_never_a_crash() {
    let original = std::fs::read(plain_dir().join("libdep.so")).unwrap();
    let dir = build_root().join("truncated");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("libcut.so");

    let lengths = (0..512).chain((512..original.len()).step_by(original.len() / 64 + 1));
    for length in lengths {
        std::fs::write(&path, &original[..length]).unwrap();
        let output = uxdroid(&["load", path.to_str().unwrap()]);
        assert!(
            output.status.code().is_some_and(|code| code <= 1),
            "truncated at {length}: {:?}\n{}",
            output.status,
            stderr(&output)
        );
    }
}
