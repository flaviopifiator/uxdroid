//! The natives of `dev.uxdroid.system.Control`: the app's side of the instance lock and the control
//! socket. Commands cross to the main Looper through an eventfd the Looper watches: a connection thread
//! queues its command and waits; the Looper takes it, runs it, and answers.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jni_sys::{JNIEnv, jboolean, jclass, jint, jlong, jobjectArray, jstring};
use serde_json::Value;

use super::agent::Agent;
use super::client::{self, CallError};
use super::protocol::{self, Command, Failure, Intent};
use super::server::{self, Bound, Handler, Info, Server};
use crate::surface::DisplaySeat;
use crate::{Method, java_string, new_string, register, throw};

/// How long a command waits for the main thread, which may be busy drawing or stuck.
const MAIN_THREAD_WAIT: Duration = Duration::from_secs(10);
/// How long a stop the main thread never took leaves the answer to reach the caller before the
/// process ends.
const ANSWER_GRACE: Duration = Duration::from_millis(300);

/// The instance lock, held until the process ends.
static INSTANCE: Mutex<Option<File>> = Mutex::new(None);
static CONTROL: Mutex<Option<(Server, Arc<Queue>)>> = Mutex::new(None);

type Answer = Result<Value, Failure>;

/// Commands waiting for the main Looper.
struct Queue {
    pending: Mutex<VecDeque<(u64, Command)>>,
    waiting: Mutex<HashMap<u64, Sender<Answer>>>,
    event: OwnedFd,
    next: AtomicU64,
    closed: AtomicBool,
    wait: Duration,
    /// Ends the process when its main thread is stuck and a stop was asked for.
    end: fn(),
}

/// What Android does to an app that stops answering: the process is killed, and its activities
/// get no callbacks.
fn end_process() {
    eprintln!(
        "uxdroid: the main thread did not take stop within {} s, so the process is killed",
        MAIN_THREAD_WAIT.as_secs()
    );
    // SAFETY: signals this process only.
    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl Queue {
    fn signal(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: an eventfd takes an 8-byte counter; a full counter only means it is already signaled.
        unsafe { libc::write(self.event.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Fails every command still waiting, so no connection waits for a Looper that has stopped.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        lock(&self.pending).clear();
        for (_, sender) in lock(&self.waiting).drain() {
            let _ = sender.send(Err(stopping()));
        }
    }
}

fn stopping() -> Failure {
    Failure::new(protocol::BUSY, "the app is stopping")
}

impl Handler for Queue {
    fn handle(&self, command: Command) -> Answer {
        if self.closed.load(Ordering::Acquire) {
            return Err(stopping());
        }
        let stop = matches!(command, Command::Stop);
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        lock(&self.waiting).insert(sequence, sender);
        lock(&self.pending).push_back((sequence, command));
        self.signal();
        match receiver.recv_timeout(self.wait) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                lock(&self.waiting).remove(&sequence);
                lock(&self.pending).retain(|(queued, _)| *queued != sequence);
                if stop {
                    // A stuck app could otherwise only be killed by hand.
                    let end = self.end;
                    std::thread::spawn(move || {
                        std::thread::sleep(ANSWER_GRACE);
                        end();
                    });
                    return Ok(serde_json::json!({ "pid": std::process::id(), "finished": 0 }));
                }
                Err(Failure::new(
                    protocol::BUSY,
                    &format!(
                        "the app's main thread did not answer within {} s",
                        self.wait.as_secs()
                    ),
                ))
            }
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `text` a string or null.
unsafe fn argument(env: *mut JNIEnv, text: jstring, what: &str) -> Option<String> {
    // SAFETY: as above.
    let value = unsafe { java_string(env, text) };
    if value.is_none() {
        // SAFETY: as above.
        unsafe {
            throw(
                env,
                c"java/lang/NullPointerException",
                &format!("no {what}"),
            )
        };
    }
    value
}

/// `static native boolean claim(String lockFile)`: takes the instance lock; false if another
/// process holds it.
unsafe extern "system" fn claim(env: *mut JNIEnv, _class: jclass, path: jstring) -> jboolean {
    // SAFETY: ART passes a valid environment.
    let Some(path) = (unsafe { argument(env, path, "lock file") }) else {
        return false;
    };
    match super::claim_instance(std::path::Path::new(&path)) {
        Ok(Some(file)) => {
            *lock(&INSTANCE) = Some(file);
            true
        }
        Ok(None) => false,
        Err(error) => {
            // SAFETY: as above.
            unsafe {
                throw(
                    env,
                    c"java/lang/IllegalStateException",
                    &format!("cannot lock {path}: {error}"),
                )
            };
            false
        }
    }
}

/// `static native int serve(String package)`: binds the package's socket and serves it; returns the
/// descriptor the main Looper watches for commands.
unsafe extern "system" fn serve(env: *mut JNIEnv, _class: jclass, package: jstring) -> jint {
    // SAFETY: ART passes a valid environment.
    let Some(package) = (unsafe { argument(env, package, "package") }) else {
        return -1;
    };
    match start(&package) {
        Ok(event) => event,
        Err(problem) => {
            // SAFETY: as above.
            unsafe { throw(env, c"java/lang/IllegalStateException", &problem) };
            -1
        }
    }
}

fn start(package: &str) -> Result<jint, String> {
    let mut control = lock(&CONTROL);
    if control.is_some() {
        return Err("the control socket is already served".into());
    }
    let profile = super::profile_from_env()?;
    let dir = super::control_dir()?;
    // Started by systemd through the app's socket unit: its socket is served, not bound again.
    let bound = match server::activated(&dir, package, &profile)? {
        Some(listener) => Bound::Listening(listener),
        None => server::bind_instance(&dir, package, &profile)?,
    };
    let listener = match bound {
        Bound::Listening(listener) => listener,
        Bound::Managed => {
            return Err(format!(
                "{} belongs to the app's systemd socket unit; start the app through it, \
                 with uxdroid app {package} start",
                super::instance_socket(&dir, package, &profile)?.display()
            ));
        }
        Bound::Owned => {
            return Err(format!(
                "{} is served by another instance of {package}, over another data directory; \
                 give each data directory its own profile",
                super::instance_socket(&dir, package, &profile)?.display()
            ));
        }
    };
    // SAFETY: eventfd has no memory preconditions; its result is checked before it is owned.
    let event = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if event < 0 {
        return Err(format!(
            "cannot make an eventfd: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `event` is a descriptor nothing else owns.
    let event = unsafe { OwnedFd::from_raw_fd(event) };
    let queue = Arc::new(Queue {
        pending: Mutex::default(),
        waiting: Mutex::default(),
        event,
        next: AtomicU64::new(1),
        closed: AtomicBool::new(false),
        wait: MAIN_THREAD_WAIT,
        end: end_process,
    });
    let info = Info {
        package: package.to_string(),
        pid: std::process::id(),
    };
    // Agent commands act through the display's seat; the rest, and `ui`, go to the main Looper.
    let agent = Arc::new(Agent::new(queue.clone(), Arc::new(DisplaySeat)));
    let server = server::serve(listener, info, super::euid(), agent)
        .map_err(|error| format!("cannot start the control thread: {error}"))?;
    let raw = queue.event.as_raw_fd();
    *control = Some((server, queue));
    Ok(raw)
}

/// `static native String[] take()`: the next command as {sequence, name, arguments JSON or null}, or
/// null: a start's or intent's intent, or another command's fields.
unsafe extern "system" fn take(env: *mut JNIEnv, _class: jclass) -> jobjectArray {
    let Some(queue) = lock(&CONTROL).as_ref().map(|(_, queue)| queue.clone()) else {
        return std::ptr::null_mut();
    };
    let mut counter = [0u8; 8];
    // SAFETY: reads the 8-byte counter, which resets it; the descriptor does not block.
    unsafe {
        libc::read(
            queue.event.as_raw_fd(),
            counter.as_mut_ptr().cast(),
            counter.len(),
        )
    };
    let Some((sequence, command)) = lock(&queue.pending).pop_front() else {
        return std::ptr::null_mut();
    };
    // More may be queued behind this one; the Looper comes back for it.
    if !lock(&queue.pending).is_empty() {
        queue.signal();
    }
    let arguments = match &command {
        Command::Start(intent) | Command::Intent(intent) => Some(intent.to_json().to_string()),
        other => Some(Value::Object(other.fields()).to_string()),
    };
    let fields = [
        Some(sequence.to_string()),
        Some(command.name().to_string()),
        arguments,
    ];
    // SAFETY: ART passes a valid environment; String is a boot class.
    unsafe {
        let jni = &**env;
        let string = (jni.v1_1.FindClass)(env, c"java/lang/String".as_ptr());
        let array =
            (jni.v1_1.NewObjectArray)(env, fields.len() as jint, string, std::ptr::null_mut());
        if array.is_null() {
            return array;
        }
        for (index, field) in fields.iter().enumerate() {
            if let Some(field) = field {
                (jni.v1_1.SetObjectArrayElement)(env, array, index as jint, new_string(env, field));
            }
        }
        array
    }
}

/// `static native void answer(long sequence, String result, String code, String message)`: the
/// result as JSON, or the code and message of a refusal when the result is null.
unsafe extern "system" fn answer(
    env: *mut JNIEnv,
    _class: jclass,
    sequence: jlong,
    result: jstring,
    code: jstring,
    message: jstring,
) {
    // SAFETY: ART passes a valid environment and strings or nulls.
    let (result, code, message) = unsafe {
        (
            java_string(env, result),
            java_string(env, code),
            java_string(env, message),
        )
    };
    let answer = match result {
        Some(result) => serde_json::from_str(&result).map_err(|error| {
            Failure::new(
                protocol::FAILED,
                &format!("the app answered no JSON: {error}"),
            )
        }),
        None => Err(Failure::new(
            code.as_deref().unwrap_or(protocol::FAILED),
            message.as_deref().unwrap_or("the app could not do it"),
        )),
    };
    let Some(queue) = lock(&CONTROL).as_ref().map(|(_, queue)| queue.clone()) else {
        return;
    };
    if let Some(sender) = lock(&queue.waiting).remove(&(sequence as u64)) {
        let _ = sender.send(answer);
    }
}

/// `static native void close()`: stops serving and removes the socket; the instance lock stays until
/// the process ends.
unsafe extern "system" fn close(_env: *mut JNIEnv, _class: jclass) {
    let control = lock(&CONTROL).take();
    if let Some((server, queue)) = control {
        queue.close();
        server.shutdown();
    }
}

/// `static native String intent(String json)`: the intent as the protocol accepts it, or an
/// IllegalArgumentException saying why not.
unsafe extern "system" fn intent(env: *mut JNIEnv, _class: jclass, json: jstring) -> jstring {
    // SAFETY: ART passes a valid environment.
    let Some(json) = (unsafe { argument(env, json, "intent") }) else {
        return std::ptr::null_mut();
    };
    let parsed = serde_json::from_str::<Value>(&json)
        .map_err(|error| error.to_string())
        .and_then(|value| Intent::from_json(&value).map_err(|failure| failure.message));
    match parsed {
        // SAFETY: as above.
        Ok(intent) => unsafe { new_string(env, &intent.to_json().to_string()) },
        Err(problem) => {
            // SAFETY: as above.
            unsafe {
                throw(
                    env,
                    c"java/lang/IllegalArgumentException",
                    &format!("bad intent: {problem}"),
                )
            };
            std::ptr::null_mut()
        }
    }
}

/// `static native String forward(String package, String intent, int timeoutMs)`: hands a launch to the
/// running instance; returns its answer's result as JSON. The instance may still be starting, so a
/// socket that is not there yet is tried again until the timeout.
unsafe extern "system" fn forward(
    env: *mut JNIEnv,
    _class: jclass,
    package: jstring,
    intent: jstring,
    timeout: jint,
) -> jstring {
    // SAFETY: ART passes a valid environment.
    let (Some(package), Some(intent)) = (unsafe { argument(env, package, "package") }, unsafe {
        argument(env, intent, "intent")
    }) else {
        return std::ptr::null_mut();
    };
    let deadline = Instant::now() + Duration::from_millis(timeout.max(0) as u64);
    match call_running(&package, &intent, deadline) {
        // SAFETY: as above.
        Ok(result) => unsafe { new_string(env, &result.to_string()) },
        Err(problem) => {
            // SAFETY: as above.
            unsafe { throw(env, c"java/lang/IllegalStateException", &problem) };
            std::ptr::null_mut()
        }
    }
}

fn call_running(package: &str, intent: &str, deadline: Instant) -> Result<Value, String> {
    let intent = serde_json::from_str::<Value>(intent)
        .map_err(|error| error.to_string())
        .and_then(|value| Intent::from_json(&value).map_err(|failure| failure.message))?;
    let profile = super::profile_from_env()?;
    let path = super::instance_socket(&super::control_dir()?, package, &profile)?;
    let stream = loop {
        match client::connect(&path) {
            Ok(stream) => break stream,
            Err(CallError::NotRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    };
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    match client::call_on(&stream, &Command::Start(intent), left) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(failure)) => Err(format!("{} ({})", failure.message, failure.code)),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// `static native boolean activated()`: whether a service manager passed this process its control
/// socket, so that a launch cannot be handed to another process through it.
unsafe extern "system" fn activated(_env: *mut JNIEnv, _class: jclass) -> jboolean {
    let read = |name: &str| std::env::var(name).ok();
    let (pid, fds, names) = (
        read("LISTEN_PID"),
        read("LISTEN_FDS"),
        read("LISTEN_FDNAMES"),
    );
    matches!(
        server::listen_fd(
            std::process::id(),
            pid.as_deref(),
            fds.as_deref(),
            names.as_deref()
        ),
        Ok(Some(_))
    )
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 8] = [
        (c"claim", c"(Ljava/lang/String;)Z", f!(claim)),
        (c"serve", c"(Ljava/lang/String;)I", f!(serve)),
        (c"take", c"()[Ljava/lang/String;", f!(take)),
        (
            c"answer",
            c"(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            f!(answer),
        ),
        (c"close", c"()V", f!(close)),
        (c"activated", c"()Z", f!(activated)),
        (
            c"intent",
            c"(Ljava/lang/String;)Ljava/lang/String;",
            f!(intent),
        ),
        (
            c"forward",
            c"(Ljava/lang/String;Ljava/lang/String;I)Ljava/lang/String;",
            f!(forward),
        ),
    ];
    // SAFETY: the table matches the Java declarations.
    unsafe { register(env, c"dev/uxdroid/system/Control", &methods) }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENDED: AtomicBool = AtomicBool::new(false);

    fn ended() {
        ENDED.store(true, Ordering::SeqCst);
    }

    /// A queue whose main Looper never takes a command, as when the main thread is stuck.
    fn stuck() -> Queue {
        // SAFETY: makes a new descriptor.
        let event = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(event >= 0);
        Queue {
            pending: Mutex::default(),
            waiting: Mutex::default(),
            // SAFETY: `event` is a descriptor nothing else owns.
            event: unsafe { OwnedFd::from_raw_fd(event) },
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            wait: Duration::from_millis(50),
            end: ended,
        }
    }

    #[test]
    fn a_stop_the_stuck_main_thread_never_takes_ends_the_process_after_answering() {
        let answer = stuck().handle(Command::Stop).expect("stop answers");
        assert_eq!(answer["pid"], std::process::id());
        assert_eq!(answer["finished"], 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ENDED.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the process was not ended");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn other_commands_to_a_stuck_main_thread_are_still_refused_as_busy() {
        let queue = stuck();
        let failure = queue
            .handle(Command::Status)
            .expect_err("status is refused");
        assert_eq!(failure.code, protocol::BUSY);
        assert!(
            failure.message.contains("did not answer"),
            "{}",
            failure.message
        );
        assert!(lock(&queue.pending).is_empty());
    }
}
