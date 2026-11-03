//! The instance lock, the control directory and the control server, through their public interface.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::client::{self, CallError};
use super::protocol::{Command, Failure, MAX_LINE};
use super::server::{self, Bound, Handler, Info};
use super::{
    DEFAULT_PROFILE, claim_instance, control_dir_in, instance_name, instance_socket, socket_path,
    valid_profile,
};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "uxdroid-control-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

// Paths and locks

#[test]
fn the_control_directory_is_private_to_the_user() {
    let base = scratch("dir");
    let dir = control_dir_in(&base).unwrap();
    assert_eq!(dir, base.join("uxdroid"));
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    // An existing directory that others can enter is refused, not repaired behind their back.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = control_dir_in(&base).unwrap_err();
    assert!(error.contains("other users"), "{error}");
    std::fs::remove_dir(&dir).unwrap();
    std::os::unix::fs::symlink(&base, &dir).unwrap();
    assert!(control_dir_in(&base).is_err());
}

#[test]
fn socket_paths_are_for_valid_packages_only() {
    let dir = Path::new("/run/user/1000/uxdroid");
    assert_eq!(
        socket_path(dir, "com.example.app").unwrap(),
        dir.join("com.example.app.sock")
    );
    for bad in ["", "../x", "a/b", ".hidden", "a b"] {
        assert!(socket_path(dir, bad).is_err(), "{bad}");
    }
    // A Unix socket path holds at most 107 bytes.
    assert!(socket_path(dir, &"a".repeat(100)).is_err());
}

#[test]
fn each_profile_has_its_own_socket_and_the_default_keeps_the_package_name() {
    let dir = Path::new("/run/user/1000/uxdroid");
    assert_eq!(
        instance_socket(dir, "com.example.app", DEFAULT_PROFILE).unwrap(),
        dir.join("com.example.app.sock")
    );
    assert_eq!(
        instance_socket(dir, "com.example.app", "work").unwrap(),
        dir.join("com.example.app@work.sock")
    );
    assert_eq!(
        instance_name("com.example.app", "work"),
        "com.example.app@work"
    );
    assert_eq!(
        instance_name("com.example.app", "default"),
        "com.example.app"
    );
    for bad in ["", "../x", "a/b", ".hidden", "a b", "a@b"] {
        assert!(!valid_profile(bad), "{bad}");
        assert!(
            instance_socket(dir, "com.example.app", bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn two_profiles_of_one_package_are_served_at_once() {
    let dir = scratch("profiles");
    let Bound::Listening(first) =
        server::bind_instance(&dir, "com.example.app", "default").unwrap()
    else {
        panic!("the default profile's socket is taken");
    };
    let Bound::Listening(second) = server::bind_instance(&dir, "com.example.app", "work").unwrap()
    else {
        panic!("a second profile must get its own socket");
    };
    assert_eq!(first.path(), dir.join("com.example.app.sock"));
    assert_eq!(second.path(), dir.join("com.example.app@work.sock"));
    assert!(matches!(
        server::bind_instance(&dir, "com.example.app", "work").unwrap(),
        Bound::Owned
    ));
}

#[test]
fn one_instance_holds_the_lock_until_its_file_closes() {
    let dir = scratch("lock");
    let path = dir.join("app/.com.example.running");
    let held = claim_instance(&path)
        .unwrap()
        .expect("the first claim holds the lock");
    assert!(
        claim_instance(&path).unwrap().is_none(),
        "a second claim must fail while the first holds it"
    );
    drop(held);
    assert!(
        claim_instance(&path).unwrap().is_some(),
        "the lock is free once its holder is gone"
    );
}

#[test]
fn a_lock_dies_with_its_process() {
    let dir = scratch("lock-process");
    let path = dir.join(".com.example.running");
    std::fs::write(&path, b"").unwrap();
    // One process that holds the lock and is then killed, as a crash would: the shell locks with
    // flock(1) from util-linux and becomes `sleep`, which keeps the descriptor.
    let mut holder = std::process::Command::new("sh")
        .args([
            "-c",
            "exec 9>>\"$0\"; flock --exclusive 9; exec sleep 30",
            path.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if claim_instance(&path).unwrap().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        claim_instance(&path).unwrap().is_none(),
        "the other process holds the lock"
    );
    holder.kill().unwrap();
    holder.wait().unwrap();
    let mut free = false;
    for _ in 0..100 {
        if claim_instance(&path).unwrap().is_some() {
            free = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(free, "a killed holder leaves no stale lock");
}

// The server

struct Recorder(Mutex<Vec<Command>>);

impl Handler for Recorder {
    fn handle(&self, command: Command) -> Result<Value, Failure> {
        let answer = match &command {
            Command::Status => Ok(json!({"activities": []})),
            Command::Stop => Err(Failure::new("failed", "refused by the test")),
            _ => Ok(json!({"outcome": "launched"})),
        };
        self.0.lock().unwrap().push(command);
        answer
    }
}

fn serve(name: &str, allowed_uid: u32) -> (server::Server, PathBuf, Arc<Recorder>) {
    let dir = scratch(name);
    let Bound::Listening(listener) = server::bind(&dir, "com.example.app").unwrap() else {
        panic!("the socket is free");
    };
    let path = listener.path().to_path_buf();
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let info = Info {
        package: "com.example.app".into(),
        pid: 4242,
    };
    let server = server::serve(listener, info, allowed_uid, recorder.clone()).unwrap();
    (server, path, recorder)
}

fn exchange(path: &Path, lines: &[&[u8]]) -> Vec<Value> {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    for line in lines {
        stream.write_all(line).unwrap();
        stream.write_all(b"\n").unwrap();
    }
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    BufReader::new(stream)
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect()
}

#[test]
fn the_socket_is_private_and_answers_each_line_in_order() {
    let (server, path, recorder) = serve("serve", uid());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let answers = exchange(
        &path,
        &[
            br#"{"v":1,"id":1,"cmd":"hello"}"#,
            br#"{"v":1,"id":2,"cmd":"status"}"#,
            b"garbage",
            br#"{"v":1,"id":3,"cmd":"stop"}"#,
        ],
    );
    assert_eq!(answers.len(), 4, "{answers:?}");
    assert_eq!(answers[0]["id"], 1);
    assert_eq!(answers[0]["ok"], true);
    assert_eq!(answers[0]["result"]["package"], "com.example.app");
    assert_eq!(answers[0]["result"]["pid"], 4242);
    assert_eq!(answers[0]["result"]["versions"], json!([1]));
    assert_eq!(
        answers[1],
        json!({"v": 1, "id": 2, "ok": true, "result": {"activities": []}})
    );
    assert_eq!(answers[2]["ok"], false);
    assert_eq!(answers[2]["error"]["code"], "bad_request");
    assert_eq!(
        answers[3]["error"],
        json!({"code": "failed", "message": "refused by the test"})
    );
    // Hello is answered by the server itself; the rest reached the handler.
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![Command::Status, Command::Stop]
    );
    server.shutdown();
    assert!(!path.exists(), "a clean shutdown removes the socket");
}

#[test]
fn an_oversized_line_is_refused_and_the_connection_closed() {
    let (server, path, recorder) = serve("oversized", uid());
    let huge = vec![b'{'; MAX_LINE * 2];
    let mut stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // The server may close before reading everything; a failed write is fine.
    let _ = stream.write_all(&huge);
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).unwrap();
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(answer["error"]["code"], "too_long");
    assert!(recorder.0.lock().unwrap().is_empty());
    server.shutdown();
}

#[test]
fn other_users_are_refused() {
    // The server is told to accept another uid than this test's, so this test is the other user.
    let (server, path, recorder) = serve("peer", uid().wrapping_add(1));
    let answers = exchange(&path, &[br#"{"v":1,"id":1,"cmd":"status"}"#]);
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["error"]["code"], "not_allowed");
    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "nothing from another user reaches the app"
    );
    server.shutdown();
}

#[test]
fn a_live_socket_is_not_taken_over_and_a_stale_one_is_replaced() {
    let (server, path, _) = serve("takeover", uid());
    let dir = path.parent().unwrap().to_path_buf();
    assert!(matches!(
        server::bind(&dir, "com.example.app").unwrap(),
        Bound::Owned
    ));
    // The owner is gone but left its socket file, as a crash does.
    std::mem::forget(server);
    let dir2 = scratch("stale");
    let stale = dir2.join("com.example.app.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(stale.exists());
    let Bound::Listening(listener) = server::bind(&dir2, "com.example.app").unwrap() else {
        panic!("a stale socket must be replaced");
    };
    assert_eq!(listener.path(), stale);
}

#[test]
fn the_client_calls_and_reports_refusals() {
    let (server, path, _) = serve("client", uid());
    assert_eq!(
        client::call(&path, &Command::Status, Duration::from_secs(5)).unwrap(),
        Ok(json!({"activities": []}))
    );
    let refused = client::call(&path, &Command::Stop, Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, "failed");
    server.shutdown();
    assert!(matches!(
        client::call(&path, &Command::Status, Duration::from_secs(1)),
        Err(CallError::NotRunning)
    ));
}

#[test]
fn the_client_refuses_a_hostile_server() {
    let dir = scratch("hostile");
    let path = dir.join("x.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        stream.write_all(&vec![b'['; MAX_LINE * 2]).unwrap_or(());
    });
    let error = client::call(&path, &Command::Status, Duration::from_secs(5)).unwrap_err();
    assert!(matches!(error, CallError::Protocol(_)), "{error:?}");
    thread.join().unwrap();
}
