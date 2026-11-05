//! `uxdroid install` verifies and installs its own copy of each file given, read once, so the bytes
//! verified are the bytes installed.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use uxdroid_testkit::{Apk, Key, sample_manifest};

#[test]
fn an_apk_given_as_a_pipe_installs_because_it_is_read_only_once() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("install-copy-pipe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data");
    let pipe = dir.join("app.apk");
    let status = Command::new("mkfifo").arg(&pipe).status().unwrap();
    assert!(status.success());
    let bytes = Apk::new()
        .manifest(&sample_manifest())
        .signed_bytes(&Key::test());
    // A pipe can be read only once and not seeked, so only an installer that reads the file given
    // once, into a copy of its own, installs it; the build before this read it four times.
    let writer = {
        let pipe = pipe.clone();
        std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new().write(true).open(&pipe).unwrap();
            let _ = file.write_all(&bytes);
        })
    };

    let output = Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .arg("install")
        .arg(&pipe)
        .arg("--data")
        .arg(&data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "/nonexistent")
        .output()
        .unwrap();
    writer.join().unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "installed com.example.app: 0 native libraries for x86_64 in {}\n",
            data.join("data/com.example.app/lib").display()
        )
    );
}

fn install(given: &std::path::Path, data: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .arg("install")
        .arg(given)
        .arg("--data")
        .arg(data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "/nonexistent")
        .output()
        .unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("install-copy-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_directory_given_is_named_as_one_and_nothing_is_written() {
    let dir = scratch("directory");
    let given = dir.join("apps");
    std::fs::create_dir(&given).unwrap();
    let data = dir.join("data");

    let output = install(&given, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "error: cannot read APK {}: Is a directory (os error 21)\n",
            given.display()
        )
    );
    assert!(!data.exists());
}

#[test]
fn a_file_larger_than_an_apk_may_be_is_refused_before_it_is_copied() {
    let dir = scratch("huge");
    let given = dir.join("huge.apk");
    // Sparse: it takes no disk, and an installer that copied it would write 4 GiB.
    std::fs::File::create(&given)
        .unwrap()
        .set_len(4 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let data = dir.join("data");

    let output = install(&given, &data);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "error: {} is larger than the 4294967296 bytes an APK or bundle may have, so it is refused\n",
            given.display()
        )
    );
    assert!(!data.exists());
}
