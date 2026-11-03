//! Serves the control protocol on a Unix socket: one thread accepts, one thread per connection reads
//! its lines in order. Commands other than `hello` go to a `Handler`, which in an app runs them on the
//! main Looper.

use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{self, Command, Failure, LineError};
use super::{claim_instance, socket_path};

/// Connections served at once; one more is told the app is busy.
const MAX_CONNECTIONS: usize = 16;
/// A connection that sends nothing for this long is closed.
const IDLE: Duration = Duration::from_secs(30);
/// How long a shutdown waits for connections to finish what they are answering.
const DRAIN: Duration = Duration::from_secs(2);

/// Runs the commands a server reads. Called on connection threads, so it must be shareable.
pub trait Handler: Send + Sync {
    fn handle(&self, command: Command) -> Result<Value, Failure>;
}

/// What `hello` reports without asking the app.
#[derive(Clone, Debug)]
pub struct Info {
    pub package: String,
    pub pid: u32,
}

/// A bound socket, and the lock that makes this process its only owner.
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
    _owner: File,
}

impl Listener {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub enum Bound {
    Listening(Listener),
    /// A live process owns the package's socket: an instance over another data directory.
    Owned,
}

/// Binds `<dir>/<package>.sock`. Ownership is a lock beside it, `<package>.lock`, held as long as the
/// socket is served; a socket file nobody holds the lock of was left by a process that died, and is
/// replaced.
pub fn bind(dir: &Path, package: &str) -> Result<Bound, String> {
    let path = socket_path(dir, package)?;
    let lock = dir.join(format!("{package}.lock"));
    let owner = match claim_instance(&lock) {
        Ok(Some(owner)) => owner,
        Ok(None) => return Ok(Bound::Owned),
        Err(error) => return Err(format!("cannot lock {}: {error}", lock.display())),
    };
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(format!(
                "cannot remove the stale socket {}: {error}",
                path.display()
            ));
        }
        _ => {}
    }
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("cannot bind {}: {error}", path.display()))?;
    // The directory already shuts out other users; the mode says the same of the socket itself.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot restrict {}: {error}", path.display()))?;
    Ok(Bound::Listening(Listener {
        listener,
        path,
        _owner: owner,
    }))
}

type Connections = Arc<Mutex<Vec<(UnixStream, JoinHandle<()>)>>>;

/// A running server. Dropping it without `shutdown` leaves it serving until the process ends.
pub struct Server {
    waker: UnixStream,
    accept: Option<JoinHandle<()>>,
    connections: Connections,
    path: PathBuf,
    _owner: File,
}

/// Serves `listener` until `shutdown`, accepting only peers whose uid is `allowed_uid`.
pub fn serve(
    listener: Listener,
    info: Info,
    allowed_uid: u32,
    handler: Arc<dyn Handler>,
) -> std::io::Result<Server> {
    let (waker, woken) = UnixStream::pair()?;
    let connections: Connections = Arc::default();
    let Listener {
        listener,
        path,
        _owner,
    } = listener;
    let accepted = connections.clone();
    let accept = std::thread::Builder::new()
        .name("uxdroid-control".into())
        .spawn(move || {
            accept_loop(&listener, &woken, &accepted, &info, allowed_uid, &handler);
        })?;
    Ok(Server {
        waker,
        accept: Some(accept),
        connections,
        path,
        _owner,
    })
}

impl Server {
    /// Stops accepting, lets each connection finish the answer it is writing, and removes the socket.
    pub fn shutdown(mut self) {
        let _ = send_all(&self.waker, b"x");
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let connections =
            std::mem::take(&mut *self.connections.lock().unwrap_or_else(|p| p.into_inner()));
        for (stream, _) in &connections {
            let _ = stream.shutdown(std::net::Shutdown::Read);
        }
        let deadline = Instant::now() + DRAIN;
        for (_, thread) in connections {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn accept_loop(
    listener: &UnixListener,
    woken: &UnixStream,
    connections: &Connections,
    info: &Info,
    allowed_uid: u32,
    handler: &Arc<dyn Handler>,
) {
    let mut polled = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: woken.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: both descriptors stay open for the loop's life.
        let ready = unsafe { libc::poll(polled.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if polled[1].revents != 0 {
            return;
        }
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        let mut list = connections.lock().unwrap_or_else(|p| p.into_inner());
        list.retain(|(_, thread)| !thread.is_finished());
        if list.len() >= MAX_CONNECTIONS {
            let failure = Failure::new(
                protocol::BUSY,
                "too many connections to this app; try again",
            );
            let _ = send_all(&stream, protocol::response_error(None, &failure).as_bytes());
            continue;
        }
        let Ok(kept) = stream.try_clone() else {
            continue;
        };
        let (info, handler) = (info.clone(), handler.clone());
        let spawned = std::thread::Builder::new()
            .name("uxdroid-control-connection".into())
            .spawn(move || serve_connection(stream, &info, allowed_uid, handler.as_ref()));
        if let Ok(thread) = spawned {
            list.push((kept, thread));
        }
    }
}

/// The uid of the process at the other end, from the kernel.
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer and its length match SO_PEERCRED's struct.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    (result == 0).then_some(credentials.uid)
}

/// Writes all of `bytes` without raising SIGPIPE, which would end the app, when the peer has gone.
pub(super) fn send_all(stream: &UnixStream, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: the pointer and length describe `bytes`.
        let sent = unsafe {
            libc::send(
                stream.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        bytes = &bytes[sent as usize..];
    }
    Ok(())
}

fn serve_connection(stream: UnixStream, info: &Info, allowed_uid: u32, handler: &dyn Handler) {
    converse(&stream, info, allowed_uid, handler);
    // The server keeps a copy of this socket to interrupt it, so closing ours would not end it.
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn converse(stream: &UnixStream, info: &Info, allowed_uid: u32, handler: &dyn Handler) {
    if peer_uid(stream) != Some(allowed_uid) {
        let failure = Failure::new(
            protocol::NOT_ALLOWED,
            "only the user running this app may control it",
        );
        let _ = send_all(stream, protocol::response_error(None, &failure).as_bytes());
        return;
    }
    let _ = stream.set_read_timeout(Some(IDLE));
    let mut reader = BufReader::new(stream);
    loop {
        let answer = match protocol::read_line(&mut reader) {
            Ok(Some(line)) => answer(&line, info, handler),
            Ok(None) | Err(LineError::Io(_)) => return,
            Err(LineError::TooLong) => {
                let failure = Failure::new(
                    protocol::TOO_LONG,
                    &format!("a request holds at most {} bytes", protocol::MAX_LINE),
                );
                let _ = send_all(stream, protocol::response_error(None, &failure).as_bytes());
                return;
            }
        };
        if send_all(stream, answer.as_bytes()).is_err() {
            return;
        }
    }
}

fn answer(line: &[u8], info: &Info, handler: &dyn Handler) -> String {
    let request = match protocol::parse_request(line) {
        Ok(request) => request,
        Err((id, failure)) => return protocol::response_error(id.as_ref(), &failure),
    };
    let id = request.id.as_ref();
    let result = match request.command {
        Command::Hello => Ok(json!({
            "versions": [protocol::VERSION],
            "commands": Command::NAMES,
            "package": info.package,
            "pid": info.pid,
        })),
        command => handler.handle(command),
    };
    match result {
        Ok(result) => protocol::response_ok(id, result),
        Err(failure) => protocol::response_error(id, &failure),
    }
}
