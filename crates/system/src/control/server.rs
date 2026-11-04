//! Serves the control protocol on a Unix socket: one thread accepts, one thread per connection reads
//! its lines in order. Commands other than `hello` go to a `Handler`, which in an app runs them on the
//! main Looper.

use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{self, Command, Failure, LineError};
use super::{DEFAULT_PROFILE, claim_instance, instance_name, instance_socket};

/// Connections served at once; one more is told the app is busy.
const MAX_CONNECTIONS: usize = 16;
/// A connection that sends nothing for this long is closed.
const IDLE: Duration = Duration::from_secs(30);
/// How long a shutdown waits for connections to finish what they are answering.
const DRAIN: Duration = Duration::from_secs(2);
/// A connection whose peer takes nothing of an answer for this long is closed, so a client that
/// stopped reading does not hold a connection forever.
const WRITE_LIMIT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    IDLE
};

/// What a server knows of one connection while it lasts.
#[derive(Debug, Default)]
pub struct Session {
    /// The connection asked for the app's accessibility nodes, which stay on while it is open.
    pub agent: bool,
}

/// Runs the commands a server reads. Called on connection threads, so it must be shareable.
pub trait Handler: Send + Sync {
    fn handle(&self, command: Command) -> Result<Value, Failure>;

    /// `handle`, for a command read on the connection of `session`.
    fn handle_in(&self, _session: &mut Session, command: Command) -> Result<Value, Failure> {
        self.handle(command)
    }

    /// The connection of `session` closed.
    fn ended(&self, _session: &Session) {}
}

/// What `hello` reports without asking the app.
#[derive(Clone, Debug)]
pub struct Info {
    pub package: String,
    pub pid: u32,
}

/// A bound socket, and the lock that makes this process its only owner.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
    /// False for a socket a service manager bound and passed: its file is the manager's.
    remove: bool,
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
    /// A service manager listens on the socket to start the app on demand, as a systemd socket unit
    /// does; it is left alone.
    Managed,
}

/// How long a claim of the ownership lock is retried: `uxdroid app` takes it shared for an instant to
/// tell a socket nobody serves, and that must not make a starting app give up its socket.
const CLAIM_RETRIES: u32 = 10;
const CLAIM_PAUSE: Duration = Duration::from_millis(2);

fn claim_owner(lock: &Path) -> Result<Option<File>, String> {
    for attempt in 0..=CLAIM_RETRIES {
        match claim_instance(lock) {
            Ok(Some(owner)) => return Ok(Some(owner)),
            Ok(None) if attempt < CLAIM_RETRIES => std::thread::sleep(CLAIM_PAUSE),
            Ok(None) => return Ok(None),
            Err(error) => return Err(format!("cannot lock {}: {error}", lock.display())),
        }
    }
    Ok(None)
}

/// Whether a socket in this network namespace listens at `path`, read from `/proc/net/unix` so that
/// nothing connects: a connection to a socket unit's socket would start the app. `None` when the
/// table cannot be read.
fn listening(path: &Path) -> Option<bool> {
    Some(listening_in(&std::fs::read("/proc/net/unix").ok()?, path))
}

/// Whether `table`, laid out as `/proc/net/unix`, has a listening socket at `path`.
pub(super) fn listening_in(table: &[u8], path: &Path) -> bool {
    /// `__SO_ACCEPTCON` in the flags column: the socket listens.
    const ACCEPTING: u32 = 1 << 16;
    let wanted = path.as_os_str().as_encoded_bytes();
    table.split(|&byte| byte == b'\n').skip(1).any(|line| {
        // Num RefCount Protocol Flags Type St Inode Path, padded with spaces; the path may hold spaces.
        let mut rest = line;
        let mut fields = [&b""[..]; 7];
        for field in &mut fields {
            rest = rest.trim_ascii_start();
            let end = rest
                .iter()
                .position(|&byte| byte == b' ')
                .unwrap_or(rest.len());
            (*field, rest) = rest.split_at(end);
        }
        let flags = std::str::from_utf8(fields[3])
            .ok()
            .and_then(|flags| u32::from_str_radix(flags, 16).ok());
        flags.is_some_and(|flags| flags & ACCEPTING != 0) && rest.strip_prefix(b" ") == Some(wanted)
    })
}

/// Whether a socket file may be removed and bound again: not while a service manager listens on it,
/// nor when that cannot be told, since removing a socket unit's file leaves the unit unreachable.
pub(super) fn may_replace(listening: Option<bool>, exists: bool) -> bool {
    match listening {
        Some(listening) => !listening,
        None if exists => {
            eprintln!(
                "uxdroid: /proc/net/unix cannot be read, so the control socket that exists is kept"
            );
            false
        }
        None => true,
    }
}

/// Binds `<dir>/<package>.sock`. Ownership is a lock beside it, `<package>.lock`, held as long as the
/// socket is served; a socket file nobody holds the lock of was left by a process that died, and is
/// replaced.
pub fn bind(dir: &Path, package: &str) -> Result<Bound, String> {
    bind_instance(dir, package, DEFAULT_PROFILE)
}

/// As `bind`, for one profile of the package: `<dir>/<package>@<profile>.sock` and its lock, or the
/// package's own names for the default profile.
pub fn bind_instance(dir: &Path, package: &str, profile: &str) -> Result<Bound, String> {
    let path = instance_socket(dir, package, profile)?;
    let lock = dir.join(format!("{}.lock", instance_name(package, profile)));
    let Some(owner) = claim_owner(&lock)? else {
        return Ok(Bound::Owned);
    };
    if !may_replace(listening(&path), path.exists()) {
        return Ok(Bound::Managed);
    }
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
        remove: true,
        _owner: owner,
    }))
}

/// Upper bound on `LISTEN_FDS`; systemd passes one descriptor per listening socket of the unit.
const MAX_LISTEN_FDS: usize = 1024;
/// The first descriptor a service manager passes, `SD_LISTEN_FDS_START`.
const LISTEN_FDS_START: RawFd = 3;
/// `FileDescriptorName=` of the control socket in the units `uxdroid service` writes.
pub const LISTEN_NAME: &str = "control";

/// The descriptor of the control socket a service manager passed to process `pid`, as `sd_listen_fds`
/// reads it: none when `LISTEN_PID` is unset or names another process, such as a parent that ran this
/// one; otherwise the only descriptor, or the one `LISTEN_FDNAMES` names `control`. Anything else set
/// wrong is an error, so a misconfigured unit is reported, never guessed at.
pub fn listen_fd(
    pid: u32,
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    names: Option<&str>,
) -> Result<Option<RawFd>, String> {
    let Some(listen_pid) = listen_pid else {
        return Ok(None);
    };
    let target = listen_pid
        .parse::<u32>()
        .ok()
        .filter(|&target| target > 0)
        .ok_or_else(|| {
            format!(
                "LISTEN_PID={} is not a process id",
                listen_pid.escape_debug()
            )
        })?;
    if target != pid {
        return Ok(None);
    }
    let count = listen_fds
        .ok_or("LISTEN_PID is set without LISTEN_FDS")?
        .parse::<usize>()
        .ok()
        .filter(|count| (1..=MAX_LISTEN_FDS).contains(count))
        .ok_or_else(|| {
            format!(
                "LISTEN_FDS={} is not a number of descriptors",
                listen_fds.unwrap_or_default().escape_debug()
            )
        })?;
    let names: Option<Vec<&str>> = names.map(|names| names.split(':').collect());
    if let Some(names) = &names
        && names.len() != count
    {
        return Err(format!(
            "LISTEN_FDNAMES names {} descriptors and LISTEN_FDS passes {count}",
            names.len()
        ));
    }
    let index = match (count, &names) {
        (1, _) => 0,
        (_, None) => {
            return Err(format!(
                "{count} descriptors were passed without names; name the control socket {LISTEN_NAME}"
            ));
        }
        (_, Some(names)) => {
            let mut named = names
                .iter()
                .enumerate()
                .filter(|(_, name)| **name == LISTEN_NAME);
            match (named.next(), named.next()) {
                (Some((index, _)), None) => index,
                _ => {
                    return Err(format!(
                        "LISTEN_FDNAMES must name exactly one descriptor {LISTEN_NAME}"
                    ));
                }
            }
        }
    };
    Ok(Some(LISTEN_FDS_START + index as RawFd))
}

/// The control socket systemd passed this process, for one profile of a package, if it was started
/// through its socket unit.
pub fn activated(dir: &Path, package: &str, profile: &str) -> Result<Option<Listener>, String> {
    let read = |name: &str| std::env::var(name).ok();
    let (pid, fds, names) = (
        read("LISTEN_PID"),
        read("LISTEN_FDS"),
        read("LISTEN_FDNAMES"),
    );
    let me = std::process::id();
    // Every descriptor passed to this process closes on exec, as `sd_listen_fds` makes them, those
    // not chosen and those of a refused unit too, so no program the app runs inherits them.
    if pid.as_deref().and_then(|pid| pid.parse::<u32>().ok()) == Some(me)
        && let Some(count) = fds.as_deref().and_then(|fds| fds.parse::<usize>().ok())
    {
        close_on_exec(LISTEN_FDS_START, count.min(MAX_LISTEN_FDS));
    }
    let fd = listen_fd(me, pid.as_deref(), fds.as_deref(), names.as_deref())?;
    fd.map(|fd| adopt(dir, package, profile, fd)).transpose()
}

/// Sets close-on-exec on the `count` descriptors from `first` that are open.
pub(super) fn close_on_exec(first: RawFd, count: usize) {
    for fd in (first..).take(count) {
        // SAFETY: sets a flag of a descriptor; one that is not open fails harmlessly.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

fn socket_option(fd: RawFd, option: libc::c_int) -> Option<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe one int, which these options write.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&raw mut value).cast(),
            &mut length,
        )
    };
    (result == 0).then_some(value)
}

/// The path a Unix socket is bound to.
fn bound_path(fd: RawFd) -> Option<PathBuf> {
    // SAFETY: sockaddr_un is plain data; zero is a valid value.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe one sockaddr_un.
    let result = unsafe { libc::getsockname(fd, (&raw mut address).cast(), &mut length) };
    if result != 0 {
        return None;
    }
    let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    let used = (length as usize)
        .checked_sub(offset)?
        .min(address.sun_path.len());
    let bytes: Vec<u8> = address.sun_path[..used]
        .iter()
        .map(|&byte| byte.to_ne_bytes()[0])
        .take_while(|&byte| byte != 0)
        .collect();
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Takes `fd`, a socket a service manager bound and passed, as the control socket of one profile
/// of a package: it must be a listening Unix stream socket bound to that profile's path in `dir`.
/// The file is made private as a bound one is, the ownership lock is taken as for a bound one, and
/// the descriptor closes on exec. The descriptor is owned from here on, refused or not.
pub fn adopt(dir: &Path, package: &str, profile: &str, fd: RawFd) -> Result<Listener, String> {
    let path = instance_socket(dir, package, profile)?;
    // SAFETY: the caller hands over `fd`, which nothing else closes.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fstat writes one struct stat; zero is a valid value for it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above, on an open descriptor.
    if unsafe { libc::fstat(owned.as_raw_fd(), &mut stat) } != 0
        || stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
    {
        return Err(format!(
            "descriptor {fd}, passed as the control socket, is not a socket"
        ));
    }
    let listening = socket_option(fd, libc::SO_DOMAIN) == Some(libc::AF_UNIX)
        && socket_option(fd, libc::SO_TYPE) == Some(libc::SOCK_STREAM)
        && socket_option(fd, libc::SO_ACCEPTCONN) == Some(1);
    if !listening {
        return Err(format!(
            "descriptor {fd}, passed as the control socket, is not a listening Unix stream socket"
        ));
    }
    match bound_path(fd) {
        Some(bound) if bound == path => {}
        bound => {
            return Err(format!(
                "descriptor {fd}, passed as the control socket, is bound to {} and not to {}",
                bound.map_or("no path".into(), |bound| bound.display().to_string()),
                path.display()
            ));
        }
    }
    // SAFETY: sets the close-on-exec flag of an open descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(format!(
            "cannot keep descriptor {fd} from programs the app runs: {}",
            std::io::Error::last_os_error()
        ));
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot restrict {}: {error}", path.display()))?;
    let lock = dir.join(format!("{}.lock", instance_name(package, profile)));
    let owner = claim_owner(&lock)?.ok_or_else(|| {
        format!(
            "another process holds {} and serves {}",
            lock.display(),
            path.display()
        )
    })?;
    Ok(Listener {
        listener: UnixListener::from(owned),
        path,
        remove: false,
        _owner: owner,
    })
}

type Connections = Arc<Mutex<Vec<(UnixStream, JoinHandle<()>)>>>;

/// A running server. Dropping it without `shutdown` leaves it serving until the process ends.
pub struct Server {
    waker: UnixStream,
    accept: Option<JoinHandle<()>>,
    connections: Connections,
    path: PathBuf,
    remove: bool,
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
        remove,
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
        remove,
        _owner,
    })
}

impl Server {
    /// Stops accepting, lets each connection finish the answer it is writing, and removes the socket
    /// unless a service manager passed it.
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
        if self.remove {
            let _ = std::fs::remove_file(&self.path);
        }
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
            // Out of descriptors, say: the listener stays readable, so waiting keeps this from spinning.
            std::thread::sleep(Duration::from_millis(100));
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
    let mut session = Session::default();
    converse(&stream, info, allowed_uid, handler, &mut session);
    handler.ended(&session);
    // The server keeps a copy of this socket to interrupt it, so closing ours would not end it.
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn converse(
    stream: &UnixStream,
    info: &Info,
    allowed_uid: u32,
    handler: &dyn Handler,
    session: &mut Session,
) {
    if peer_uid(stream) != Some(allowed_uid) {
        let failure = Failure::new(
            protocol::NOT_ALLOWED,
            "only the user running this app may control it",
        );
        let _ = send_all(stream, protocol::response_error(None, &failure).as_bytes());
        return;
    }
    let _ = stream.set_read_timeout(Some(IDLE));
    let _ = stream.set_write_timeout(Some(WRITE_LIMIT));
    let mut reader = BufReader::new(stream);
    loop {
        let answer = match protocol::read_line(&mut reader) {
            Ok(Some(line)) => answer(&line, info, handler, session),
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

fn answer(line: &[u8], info: &Info, handler: &dyn Handler, session: &mut Session) -> String {
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
        command => handler.handle_in(session, command),
    };
    match result {
        Ok(result) => protocol::response_ok(id, result),
        Err(failure) => protocol::response_error(id, &failure),
    }
}
