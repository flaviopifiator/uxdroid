//! `uxdroid load` against libraries really built with the Android NDK.
//!
//! The NDK ships in the dev image. Without it these tests fail instead of skipping silently.
//! Fixtures are built for the CPU the tests run on, x86_64 or aarch64; on an x86_64 machine the
//! aarch64 build runs under QEMU user, which `UXDROID_TEST_RUNNER` names (`dev-arm64` service).
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux",
    target_env = "gnu"
))]

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
    "test_thread_objects_keep_their_bionic_size",
    "test_misaligned_conditions_and_semaphores",
    "test_dlopen",
    "test_getaddrinfo",
    "test_strerror_r",
    "test_setjmp_restores_the_signal_mask",
    "test_sigsetjmp",
    "test_path_helpers_leave_their_input_alone",
    "test_getnameinfo",
    "test_pathconf",
    "test_setbuf",
    "test_fortified_fread",
    "test_printf_family",
    "test_integer_with_l_modifier_is_not_long_double",
    "test_multibyte_text_works_by_default",
    "test_atfork",
    "test_getprogname",
    "test_ifunc",
    "test_emulated_tls",
];

/// The NDK's name for this host's CPU, whose libraries load with no translation.
#[cfg(target_arch = "x86_64")]
const HOST_TRIPLE: &str = "x86_64-linux-android";
#[cfg(target_arch = "aarch64")]
const HOST_TRIPLE: &str = "aarch64-linux-android";

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
        "bin/{HOST_TRIPLE}{sdk}-clang{}",
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
            // Every function checks its stack canary, which arm64 code reads from bionic's TLS
            // slot 5, at a fixed offset from the thread pointer.
            ("stack-protector", &["-fstack-protector-all"]),
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

/// The built `uxdroid`, started through `UXDROID_TEST_RUNNER` when that is set.
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
        "I/fixture: plain message\nD/fixture: buffer message\nW/fixture: 1 two 3.5 4 5 6 7 8 9.5\n"
    );
}

#[test]
fn the_ndk_cxx_runtime_works_shared_and_static() {
    let dir = build_root().join("cxx");
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = toolchain().join(format!("sysroot/usr/lib/{HOST_TRIPLE}/libc++_shared.so"));
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
fn unaudited_glibc_symbols_stop_the_program_by_name_when_called() {
    let library = plain_dir().join("libbasic.so");

    for (function, message) in [
        ("trap_unaudited", "uxdroid: regcomp is not audited"),
        ("trap_long_double_math", "uxdroid: sincosl is not audited"),
        ("trap_ilogb", "uxdroid: ilogb is not audited"),
        (
            "trap_log_long_double",
            "__android_log_print with a long double format",
        ),
        (
            "trap_printf_long_double",
            "with a long double format has an incompatible ABI",
        ),
    ] {
        let output = load_and_call(&library, function);

        assert_eq!(output.status.code(), None, "{function} must die from abort");
        assert!(
            stderr(&output).contains(message),
            "{function}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn unaudited_glibc_data_fails_the_load() {
    let dir = build_root().join("data");
    let source = dir.join("data.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &source,
        "extern int getdate_err;\nint entry(void) { return getdate_err; }\n",
    )
    .unwrap();
    ndk_build(
        &dir,
        "libdata.so",
        MIN_SDK,
        &[source.to_str().unwrap()],
        &["-Wl,-z,undefs"],
    );

    let output = uxdroid(&["load", dir.join("libdata.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libdata.so: unresolved symbols: getdate_err\n"
    );
}

#[test]
fn repeating_call_is_wrong_usage() {
    let output = uxdroid(&["load", "a.so", "--call", "one", "--call", "two"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("usage: uxdroid load"));
}

#[test]
fn a_bare_file_name_searches_the_current_directory_for_dependencies() {
    let output = uxdroid_command()
        .args(["load", "libbasic.so", "--call", "answer"])
        .current_dir(plain_dir())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).ends_with("call answer: 42\n"));
}

#[test]
fn calling_an_exported_ifunc_calls_the_function_it_resolves_to() {
    let dir = build_root().join("ifunc");
    let source = dir.join("ifunc.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &source,
        "static int target(void) { return 99; }\n\
         static int (*resolver(void))(void) { return target; }\n\
         int exported(void) __attribute__((ifunc(\"resolver\")));\n",
    )
    .unwrap();
    ndk_build(
        &dir,
        "libifunc.so",
        MIN_SDK,
        &[source.to_str().unwrap()],
        &[],
    );

    let output = load_and_call(&dir.join("libifunc.so"), "exported");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("call exported: 99\n"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn an_ifunc_resolver_outside_code_is_rejected_before_running() {
    let dir = build_root().join("bad-ifunc");
    let source = dir.join("bad.s");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(target_arch = "x86_64")]
    let call = "call bad@PLT\nret";
    #[cfg(target_arch = "aarch64")]
    let call = "b bad";
    std::fs::write(
        &source,
        format!(
            ".data\n.globl bad\n.type bad, @gnu_indirect_function\nbad:\n.quad 0\n\
             .text\n.globl entry\nentry:\n{call}\n"
        ),
    )
    .unwrap();
    ndk_build(&dir, "libbad.so", MIN_SDK, &[source.to_str().unwrap()], &[]);

    let output = uxdroid(&["load", dir.join("libbad.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1), "{:?}", output.status);
    assert!(
        stderr(&output).contains("ifunc resolver outside executable segments"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_same_name_at_another_path_is_a_different_library() {
    let dir = build_root().join("same-name");
    for (subdir, value) in [("a", 1), ("b", 2)] {
        let source = dir.join(format!("{subdir}.c"));
        std::fs::create_dir_all(dir.join(subdir)).unwrap();
        std::fs::write(&source, format!("int value(void) {{ return {value}; }}\n")).unwrap();
        ndk_build(
            &dir.join(subdir),
            "libsame.so",
            MIN_SDK,
            &[source.to_str().unwrap()],
            &[],
        );
    }
    let source = dir.join("top.c");
    let first = dir.join("a/libsame.so");
    let second = dir.join("b/libsame.so");
    std::fs::write(
        &source,
        format!(
            "#include <dlfcn.h>\n\
             int entry(void) {{\n\
               void *a = dlopen(\"{}\", RTLD_NOW);\n\
               void *b = dlopen(\"{}\", RTLD_NOW);\n\
               int (*fa)(void) = (int (*)(void))dlsym(a, \"value\");\n\
               int (*fb)(void) = (int (*)(void))dlsym(b, \"value\");\n\
               return fa() * 10 + fb();\n\
             }}\n",
            first.display(),
            second.display()
        ),
    )
    .unwrap();
    ndk_build(
        &dir,
        "libtop.so",
        MIN_SDK,
        &[source.to_str().unwrap()],
        &["-ldl"],
    );

    let output = load_and_call(&dir.join("libtop.so"), "entry");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("call entry: 12\n"),
        "{}",
        stdout(&output)
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

/// A library that needs Vulkan, which uxdroid has no table for, and calls one of its functions.
const VULKAN_SOURCE: &str = "#include <vulkan/vulkan.h>\n\
     int make(void) {\n\
       VkInstanceCreateInfo info = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};\n\
       VkInstance instance;\n\
       return vkCreateInstance(&info, 0, &instance);\n\
     }\n";

#[test]
fn a_library_needing_an_ndk_library_uxdroid_lacks_loads_and_stops_naming_the_call() {
    let dir = build_root().join("vulkan");
    let source = dir.join("vulkan.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&source, VULKAN_SOURCE).unwrap();
    ndk_build(
        &dir,
        "libvk.so",
        "24",
        &[source.to_str().unwrap()],
        &["-lvulkan"],
    );
    let warning = "warning: libvk.so calls functions uxdroid does not have yet, \
                   and stops if it reaches one: vkCreateInstance\n";

    let loaded = uxdroid(&["load", dir.join("libvk.so").to_str().unwrap()]);
    let called = load_and_call(&dir.join("libvk.so"), "make");

    assert_eq!(loaded.status.code(), Some(0), "{}", stderr(&loaded));
    assert_eq!(stderr(&loaded), warning);
    assert!(
        stdout(&loaded).contains("needs: libvulkan.so"),
        "{}",
        stdout(&loaded)
    );
    assert_eq!(called.status.code(), None, "must die from abort");
    // QEMU user, on an x86_64 machine testing the aarch64 build, reports the abort after it.
    assert!(
        stderr(&called).starts_with(&format!(
            "{warning}uxdroid: libvulkan.so vkCreateInstance is not implemented yet\n"
        )),
        "{}",
        stderr(&called)
    );
}

/// Only Android's official exports of a needed library are stubbed: any other name is no API.
#[test]
fn a_name_on_no_ndk_list_still_fails_the_load_beside_a_needed_ndk_library() {
    let dir = build_root().join("not-listed");
    let source = dir.join("not_listed.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &source,
        "#include <android/trace.h>\n\
         int ATrace_notAnApi(void);\n\
         int entry(void) { ATrace_beginSection(\"x\"); return ATrace_notAnApi(); }\n",
    )
    .unwrap();
    ndk_build(
        &dir,
        "libnotlisted.so",
        "24",
        &[source.to_str().unwrap()],
        &["-landroid", "-Wl,-z,undefs"],
    );

    let output = uxdroid(&["load", dir.join("libnotlisted.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libnotlisted.so: unresolved symbols: ATrace_notAnApi\n"
    );
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

/// Where an installer put the libraries can forbid executable file mappings, as the sandbox's
/// `/tmp` does; the libraries must still load. `/dev/shm` is mounted that way in the dev image.
#[test]
fn libraries_on_a_noexec_mount_still_load() {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap();
    let noexec = mounts.lines().any(|line| {
        let fields: Vec<&str> = line.split(' ').collect();
        fields.get(1) == Some(&"/dev/shm")
            && fields
                .get(3)
                .is_some_and(|options| options.split(',').any(|option| option == "noexec"))
    });
    assert!(
        noexec,
        "this test needs /dev/shm mounted noexec, as in the dev image"
    );
    let dir = Path::new("/dev/shm").join(format!("uxdroid-noexec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["libbasic.so", "libdep.so"] {
        std::fs::copy(plain_dir().join(name), dir.join(name)).unwrap();
    }

    let output = load_and_call(&dir.join("libbasic.so"), "answer");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("call answer: 42\n"),
        "{}",
        stdout(&output)
    );
}

/// Android's system partition is the runtime root that `ANDROID_ROOT` names.
#[test]
fn system_partition_paths_resolve_under_the_runtime_root() {
    let dir = build_root().join("system-path");
    ndk_build(&dir, "libsystempath.so", MIN_SDK, &["system_path.c"], &[]);
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::write(root.join("etc/uxdroid-probe"), "7").unwrap();

    let output = uxdroid_command()
        .args(["load", dir.join("libsystempath.so").to_str().unwrap()])
        .args(["--call", "read_system_probe"])
        .env("ANDROID_ROOT", &root)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("call read_system_probe: 0\n"),
        "{}",
        stdout(&output)
    );
}

/// The translation spike's arm64 library, from the bridge's fixtures.
fn arm64_spike() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = build_root().join("arm64");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("libspike.so");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridge/tests/fixtures/arm64/spike.cpp");
        let status = Command::new(toolchain().join("bin/aarch64-linux-android24-clang++"))
            .arg(source)
            .args([
                "-shared",
                "-fPIC",
                "-O2",
                "-static-libstdc++",
                "-llog",
                "-o",
            ])
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success(), "failed to build the arm64 spike");
        output
    })
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_without_its_translator_is_refused_by_name() {
    let output = uxdroid_command()
        .args(["load", arm64_spike().to_str().unwrap()])
        .env("UXDROID_TRANSLATOR", "/nonexistent/libuxdroid_dynarmic.so")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(
            "libspike.so is arm64-v8a code, which needs the translator: cannot open /nonexistent/libuxdroid_dynarmic.so"
        ),
        "{}",
        stderr(&output)
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_runs_through_the_translator() {
    if std::env::var_os("UXDROID_TRANSLATOR").is_none() {
        eprintln!("skipped: UXDROID_TRANSLATOR is not set; run in the translation service");
        return;
    }
    let output = load_and_call(arm64_spike(), "spike_qsort");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("call spike_qsort: 1235789\n"),
        "{}",
        stdout(&output)
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_arm64_library_needing_an_ndk_library_uxdroid_lacks_stops_naming_the_call() {
    if std::env::var_os("UXDROID_TRANSLATOR").is_none() {
        eprintln!("skipped: UXDROID_TRANSLATOR is not set; run in the translation service");
        return;
    }
    let dir = build_root().join("arm64-vulkan");
    let source = dir.join("vulkan.c");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&source, VULKAN_SOURCE).unwrap();
    let status = Command::new(toolchain().join("bin/aarch64-linux-android24-clang"))
        .arg(&source)
        .args(["-shared", "-fPIC", "-O2", "-lvulkan", "-o"])
        .arg(dir.join("libvk.so"))
        .status()
        .unwrap();
    assert!(status.success(), "failed to build the arm64 library");

    let output = load_and_call(&dir.join("libvk.so"), "make");

    assert_eq!(output.status.code(), None, "must die from abort");
    assert_eq!(
        stderr(&output),
        "warning: libvk.so calls functions uxdroid does not have yet, \
         and stops if it reaches one: vkCreateInstance\n\
         uxdroid: libvulkan.so vkCreateInstance is not implemented yet\n"
    );
}

/// On an arm64 host the spike's library is host code: it loads and runs with no translator,
/// even when the one named cannot be opened.
#[cfg(target_arch = "aarch64")]
#[test]
fn an_arm64_library_runs_natively_on_an_arm64_host() {
    for (function, expected) in [
        ("spike_qsort", 1_235_789),
        ("spike_dl", 11_111),
        ("spike_epoll", 1111),
        ("spike_signal", 1000 + 10 * 10 + 1),
        ("spike_stack_bounds", 11),
    ] {
        let output = uxdroid_command()
            .args(["load", arm64_spike().to_str().unwrap(), "--call", function])
            .env("UXDROID_TRANSLATOR", "/nonexistent/libuxdroid_dynarmic.so")
            .output()
            .unwrap();
        assert!(output.status.success(), "{function}: {}", stderr(&output));
        assert!(
            stdout(&output).ends_with(&format!("call {function}: {expected}\n")),
            "{function}: {}",
            stdout(&output)
        );
    }
}

/// An x86_64 library is the foreign ABI on an arm64 host, which has no translator yet.
#[cfg(target_arch = "aarch64")]
#[test]
fn an_x86_64_library_on_an_arm64_host_is_refused_by_name() {
    let dir = build_root().join("x86_64");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("foreign.c");
    std::fs::write(&source, "int entry(void) { return 1; }\n").unwrap();
    let status = Command::new(toolchain().join("bin/x86_64-linux-android24-clang"))
        .arg(&source)
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libforeign.so"))
        .status()
        .unwrap();
    assert!(status.success(), "failed to build the x86_64 library");

    let output = uxdroid(&["load", dir.join("libforeign.so").to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: libforeign.so is x86_64 code, which needs the translator: \
         an arm64 host has no translator for x86_64 code yet\n"
    );
}
