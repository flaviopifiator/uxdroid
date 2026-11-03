//! Socket activation: the control socket a service manager binds and passes, as systemd does with
//! `LISTEN_PID`, `LISTEN_FDS` and `LISTEN_FDNAMES`, through the server's public interface.

use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::client;
use super::protocol::{Command, Failure};
use super::server::{self, Bound, Handler, Info, listen_fd};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "uxdroid-activation-test-{}-{name}",
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

#[test]
fn listen_variables_are_checked() {
    let me = 4242;
    // Not activated: nothing is set, or the descriptors were meant for another process.
    assert_eq!(listen_fd(me, None, None, None), Ok(None));
    assert_eq!(listen_fd(me, None, Some("1"), None), Ok(None));
    assert_eq!(listen_fd(me, Some("4243"), Some("1"), None), Ok(None));
    // One descriptor is descriptor 3, named or not.
    assert_eq!(listen_fd(me, Some("4242"), Some("1"), None), Ok(Some(3)));
    assert_eq!(
        listen_fd(me, Some("4242"), Some("1"), Some("control")),
        Ok(Some(3))
    );
    // Several are told apart by name: the one named `control`.
    assert_eq!(
        listen_fd(me, Some("4242"), Some("3"), Some("a:b:control")),
        Ok(Some(5))
    );
    for (pid, fds, names) in [
        (Some("x"), Some("1"), None),
        (Some("-1"), Some("1"), None),
        (Some("4242"), None, None),
        (Some("4242"), Some("0"), None),
        (Some("4242"), Some("two"), None),
        (Some("4242"), Some("100000"), None),
        (Some("4242"), Some("2"), None),
        (Some("4242"), Some("2"), Some("a:b")),
        (Some("4242"), Some("2"), Some("control:control")),
        (Some("4242"), Some("2"), Some("control")),
        (Some("4242"), Some("1"), Some("a:b")),
    ] {
        assert!(
            listen_fd(me, pid, fds, names).is_err(),
            "{pid:?} {fds:?} {names:?}"
        );
    }
}

struct Echo;

impl Handler for Echo {
    fn handle(&self, command: Command) -> Result<Value, Failure> {
        Ok(json!({ "command": command.name() }))
    }
}

/// A listening socket as the service manager makes it, and the descriptor it would pass.
fn passed(path: &std::path::Path) -> (UnixListener, i32) {
    let listener = UnixListener::bind(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let fd = listener.try_clone().unwrap().into_raw_fd();
    (listener, fd)
}

#[test]
fn a_passed_socket_is_served_and_left_to_its_manager() {
    let dir = scratch("serve");
    let path = dir.join("com.example.app@work.sock");
    let (manager, fd) = passed(&path);
    let listener = server::adopt(&dir, "com.example.app", "work", fd).unwrap();
    assert_eq!(listener.path(), path);
    // A program the app runs does not inherit it.
    // SAFETY: F_GETFD only reads the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let info = Info {
        package: "com.example.app".into(),
        pid: 7,
    };
    let server = server::serve(listener, info, uid(), Arc::new(Echo)).unwrap();
    let answer = client::call(&path, &Command::Status, Duration::from_secs(5)).unwrap();
    assert_eq!(answer, Ok(json!({ "command": "status" })));
    // The instance's ownership lock is held while it serves, as for a socket it binds.
    assert!(matches!(
        server::bind_instance(&dir, "com.example.app", "work").unwrap(),
        Bound::Owned
    ));
    server.shutdown();
    // The manager owns the file and listens on it still, for the next start.
    assert!(path.exists(), "the manager's socket file was removed");
    drop(manager);
}

#[test]
fn a_passed_socket_keeps_the_peer_check() {
    let dir = scratch("peer");
    let path = dir.join("com.example.app.sock");
    let (_manager, fd) = passed(&path);
    let listener = server::adopt(&dir, "com.example.app", "default", fd).unwrap();
    let info = Info {
        package: "com.example.app".into(),
        pid: 7,
    };
    let server = server::serve(listener, info, uid() + 1, Arc::new(Echo)).unwrap();
    let answer = client::call(&path, &Command::Status, Duration::from_secs(5)).unwrap();
    assert_eq!(answer.unwrap_err().code, "not_allowed");
    server.shutdown();
}

#[test]
fn a_passed_descriptor_that_is_not_the_apps_socket_is_refused() {
    let dir = scratch("refused");
    let path = dir.join("com.example.app.sock");
    // Bound at another path.
    let (_other, fd) = passed(&dir.join("com.other.sock"));
    let error = server::adopt(&dir, "com.example.app", "default", fd).unwrap_err();
    assert!(error.contains("com.other.sock"), "{error}");
    // Not listening.
    let (stream, _) = UnixStream::pair().unwrap();
    let error =
        server::adopt(&dir, "com.example.app", "default", stream.into_raw_fd()).unwrap_err();
    assert!(error.contains("not a listening"), "{error}");
    // A datagram socket at the right path.
    let datagram = UnixDatagram::bind(&path).unwrap();
    let error =
        server::adopt(&dir, "com.example.app", "default", datagram.into_raw_fd()).unwrap_err();
    assert!(error.contains("not a listening"), "{error}");
    std::fs::remove_file(&path).unwrap();
    // Not a socket at all.
    let file: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
    let error = server::adopt(&dir, "com.example.app", "default", file.into_raw_fd()).unwrap_err();
    assert!(error.contains("not a socket"), "{error}");
    // A bad package never reaches the descriptor.
    let (_manager, fd) = passed(&path);
    assert!(server::adopt(&dir, "../x", "default", fd).is_err());
}

#[test]
fn a_passed_socket_that_others_can_open_is_made_private() {
    let dir = scratch("mode");
    let path = dir.join("com.example.app.sock");
    let (_manager, fd) = passed(&path);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    let listener = server::adopt(&dir, "com.example.app", "default", fd).unwrap();
    let mode = std::fs::metadata(listener.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_bind_leaves_a_socket_its_manager_listens_on() {
    let dir = scratch("manager");
    let path = dir.join("com.example.app.sock");
    let (manager, _) = passed(&path);
    // No instance holds the ownership lock, so only the listener tells it from a stale file.
    assert!(matches!(
        server::bind(&dir, "com.example.app").unwrap(),
        Bound::Managed
    ));
    assert!(path.exists());
    let accepted = manager
        .set_nonblocking(true)
        .and_then(|()| manager.accept());
    assert!(
        accepted.is_err(),
        "the check must not connect, which would start the app"
    );
    assert!(manager.as_raw_fd() >= 0);
}
