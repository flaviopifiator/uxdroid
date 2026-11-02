//! Writes the APK that the first Activity smoke test runs.
//! usage: make-first-apk <classes.dex> <libfirstnative.so | -> <output.apk> [extra entry name]

use std::path::Path;

use uxdroid_testkit::{ATTR_MIN_SDK, ATTR_NAME, ATTR_TARGET_SDK, Apk, Axml, Value, android, plain};

fn manifest() -> Axml {
    Axml::new()
        .start(
            "manifest",
            &[plain("package", Value::Str("com.example.first"))],
        )
        .start(
            "uses-sdk",
            &[
                android("minSdkVersion", ATTR_MIN_SDK, Value::Int(26)),
                android("targetSdkVersion", ATTR_TARGET_SDK, Value::Int(35)),
            ],
        )
        .end("uses-sdk")
        .start("application", &[])
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dex, library, output, extra) = match args.as_slice() {
        [dex, library, output] => (dex, library, output, None),
        [dex, library, output, extra] => (dex, library, output, Some(extra)),
        _ => {
            eprintln!(
                "usage: make-first-apk <classes.dex> <libfirstnative.so | -> <output.apk> [extra entry name]"
            );
            std::process::exit(2);
        }
    };
    let mut apk = Apk::new()
        .manifest(&manifest())
        .file("classes.dex", &std::fs::read(dex).unwrap());
    if library != "-" {
        apk = apk.file(
            "lib/x86_64/libfirstnative.so",
            &std::fs::read(library).unwrap(),
        );
    }
    if let Some(extra) = extra {
        apk = apk.file(extra, b"not a library");
    }
    apk.write(Path::new(output));
}
