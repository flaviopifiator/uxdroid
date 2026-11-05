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

    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "installed com.example.app: 0 native libraries for x86_64 in {}\n",
            data.join("data/com.example.app/lib").display()
        )
    );
}
