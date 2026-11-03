//! A bionic library that needs an Android system library binds to that library's signature table.
//!
//! The fixtures are built with the NDK, which the dev image has. Without `ANDROID_NDK_ROOT` these
//! tests say so and pass, since no fixture can be built.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

use uxdroid_bridge::table::{Export, Signature, Table, Type, provide, validate};
use uxdroid_bridge::{Error, load};

static PROVIDED_DATA: i32 = 7;
static CALLS: AtomicI32 = AtomicI32::new(0);

extern "C" fn provided_answer() -> i32 {
    CALLS.fetch_add(1, Ordering::Relaxed);
    42
}

static TEST_TABLE: Table = Table {
    library: "libuxdroidtest.so",
    exports: &[
        Export::function(c"provided_answer", "i32()", provided_answer as *const ()),
        Export::data(
            c"provided_data",
            "i32",
            std::ptr::from_ref(&PROVIDED_DATA).cast(),
        ),
    ],
};

/// A scratch directory outside the source tree, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("uxdroid-tables-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn clang() -> Option<PathBuf> {
    let root = std::env::var_os("ANDROID_NDK_ROOT")?;
    Some(
        Path::new(&root)
            .join("toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang"),
    )
}

fn build(clang: &Path, source: &str, output: &Path, extra: &[&str]) {
    let source_path = output.with_extension("c");
    std::fs::write(&source_path, source).unwrap();
    let status = Command::new(clang)
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(output)
        .arg(&source_path)
        .args(extra)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the NDK could not build {}",
        output.display()
    );
}

/// Builds `source` linked against a stub of `libuxdroidtest.so`, then removes the stub, so only the
/// table can satisfy the dependency.
fn fixture(scratch: &Scratch, name: &str, source: &str) -> Option<PathBuf> {
    let Some(clang) = clang() else {
        eprintln!("ANDROID_NDK_ROOT is not set: no fixture can be built");
        return None;
    };
    let stubs = scratch.0.join("stubs");
    std::fs::create_dir_all(&stubs).unwrap();
    build(
        &clang,
        "int provided_answer(void) { return 0; }\nconst int provided_data = 0;\nint not_provided(void) { return 0; }\n",
        &stubs.join("libuxdroidtest.so"),
        &[],
    );
    let library = scratch.0.join(format!("lib{name}.so"));
    let stub_dir = format!("-L{}", stubs.display());
    build(
        &clang,
        source,
        &library,
        &[&stub_dir, "-luxdroidtest", "-llog"],
    );
    std::fs::remove_dir_all(&stubs).unwrap();
    Some(library)
}

#[test]
fn needed_libraries_bind_to_their_table() {
    provide(&TEST_TABLE);
    let scratch = Scratch::new("bind");
    let source = r#"
#include <android/log.h>
extern int provided_answer(void);
extern const int provided_data;
int uses_function(void) { return provided_answer(); }
int uses_data(void) { return provided_data; }
int uses_log(void) { return __android_log_print(ANDROID_LOG_INFO, "tables", "logged %d", 5); }
"#;
    let Some(path) = fixture(&scratch, "bindtables", source) else {
        return;
    };

    let library = load(&path, &[]).unwrap();

    assert!(library.needed().contains(&"libuxdroidtest.so".to_string()));
    let before = CALLS.load(Ordering::Relaxed);
    // SAFETY: the fixture defines these as `int f(void)`.
    unsafe {
        assert_eq!(library.call_i32("uses_function"), Some(42));
        assert_eq!(library.call_i32("uses_data"), Some(7));
        assert_eq!(library.call_i32("uses_log"), Some(1));
    }
    assert_eq!(CALLS.load(Ordering::Relaxed), before + 1);
}

#[test]
fn a_name_the_table_lacks_is_reported_missing() {
    provide(&TEST_TABLE);
    let scratch = Scratch::new("missing");
    let source = "extern int not_provided(void);\nint calls(void) { return not_provided(); }\n";
    let Some(path) = fixture(&scratch, "missingtables", source) else {
        return;
    };

    let error = load(&path, &[]).unwrap_err();

    match &error {
        Error::UnresolvedSymbols(libraries) => {
            assert_eq!(libraries.len(), 1);
            assert_eq!(libraries[0].1, vec!["not_provided".to_string()]);
        }
        other => panic!("expected unresolved symbols, got {other}"),
    }
}

#[test]
fn signatures_parse_into_a_closed_vocabulary() {
    assert_eq!(
        Signature::parse("i32(i32,ptr,ptr,...)").unwrap(),
        Signature::Function {
            returns: Type::I32,
            parameters: vec![Type::I32, Type::Ptr, Type::Ptr],
            variadic: true
        }
    );
    assert_eq!(
        Signature::parse("void()").unwrap(),
        Signature::Function {
            returns: Type::Void,
            parameters: vec![],
            variadic: false
        }
    );
    assert_eq!(
        Signature::parse("ptr").unwrap(),
        Signature::Object(Type::Ptr)
    );
    for bad in [
        "int(int)",
        "i32(void)",
        "i32(...)",
        "i32(i32",
        "void",
        "i32(i32,...,i32)",
        "i32( i32)",
    ] {
        assert!(Signature::parse(bad).is_err(), "{bad} parsed");
    }
}

#[test]
fn validation_rejects_duplicates_and_mismatched_kinds() {
    validate(&TEST_TABLE).unwrap();
    static TWICE: Table = Table {
        library: "libtwice.so",
        exports: &[
            Export::function(c"f", "void()", provided_answer as *const ()),
            Export::function(c"f", "void()", provided_answer as *const ()),
        ],
    };
    assert!(validate(&TWICE).is_err());
    static DATA_AS_FUNCTION: Table = Table {
        library: "libkind.so",
        exports: &[Export::data(
            c"d",
            "i32()",
            std::ptr::from_ref(&PROVIDED_DATA).cast(),
        )],
    };
    assert!(validate(&DATA_AS_FUNCTION).is_err());
}

#[test]
fn liblog_is_global_as_app_process_makes_it_on_android() {
    let scratch = Scratch::new("global");
    let Some(clang) = clang() else {
        eprintln!("ANDROID_NDK_ROOT is not set: no fixture can be built");
        return;
    };
    // No -llog: the symbol stays undefined and no DT_NEEDED names liblog.so.
    let source = "int __android_log_write(int, const char*, const char*);\n\
                  int logs(void) { return __android_log_write(4, \"tables\", \"global\"); }\n";
    let path = scratch.0.join("libgloballog.so");
    build(&clang, source, &path, &["-Wl,-z,undefs"]);

    let library = load(&path, &[]).unwrap();

    assert!(!library.needed().contains(&"liblog.so".to_string()));
    // SAFETY: the fixture defines `int logs(void)`.
    assert_eq!(unsafe { library.call_i32("logs") }, Some(1));
}
