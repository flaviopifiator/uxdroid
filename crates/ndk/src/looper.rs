//! `ALooper`: one epoll set per thread, joined to the framework's `Looper` where a thread has one.
//!
//! Native threads, such as the ones Flutter starts, call `ALooper_prepare` and then poll here, as on
//! Android. A thread with a Java `Looper`, such as the main thread, never polls natively: it runs
//! `Looper.loop`. Its `ALooper` therefore puts its own epoll descriptor on the Java `MessageQueue`
//! through `addOnFileDescriptorEventListener`, the framework's API for exactly this, so a ready
//! descriptor wakes `Looper.loop`, and the listener, `dev.uxdroid.system.NdkLooperListener`, runs the
//! callbacks on that thread. Nested epoll keeps each side's descriptors its own.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant};

use jni_sys::{JNIEnv, jclass, jint, jlong, jvalue};

use crate::java::{Jni, Thrown};

const POLL_WAKE: c_int = -1;
const POLL_CALLBACK: c_int = -2;
const POLL_TIMEOUT: c_int = -3;
const POLL_ERROR: c_int = -4;
const PREPARE_ALLOW_NON_CALLBACKS: c_int = 1;
const EVENT_INPUT: u32 = 1;
const EVENT_OUTPUT: u32 = 2;
const EVENT_ERROR: u32 = 4;
const EVENT_HANGUP: u32 = 8;
/// `MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT`.
const JAVA_EVENT_INPUT: jint = 1;
const MAX_EVENTS: usize = 16;

type Callback = unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int;

#[derive(Clone, Copy)]
struct Request {
    ident: c_int,
    callback: Option<Callback>,
    data: *mut c_void,
    /// Tells a newer registration of the same fd from the one a callback answered for.
    sequence: u64,
}

struct Response {
    fd: c_int,
    events: u32,
    request: Request,
}

struct State {
    requests: HashMap<c_int, Request>,
    pending: VecDeque<Response>,
    sequence: u64,
}

pub struct Looper {
    epoll: c_int,
    wake: c_int,
    allow_non_callbacks: bool,
    state: Mutex<State>,
}

// SAFETY: the raw `data` pointers are app values passed back unchanged, never dereferenced here,
// and every other field is a descriptor or behind the mutex.
unsafe impl Send for Looper {}
// SAFETY: as above.
unsafe impl Sync for Looper {}

/// Every looper alive, so that pointers from app code are checked before use.
static LIVE: Mutex<BTreeMap<usize, Weak<Looper>>> = Mutex::new(BTreeMap::new());

fn live() -> std::sync::MutexGuard<'static, BTreeMap<usize, Weak<Looper>>> {
    LIVE.lock().unwrap_or_else(|p| p.into_inner())
}

impl Drop for Looper {
    fn drop(&mut self) {
        live().remove(&(std::ptr::from_ref(self) as usize));
        // SAFETY: both descriptors belong to this looper alone.
        unsafe {
            libc::close(self.wake);
            libc::close(self.epoll);
        }
    }
}

impl Looper {
    fn new(allow_non_callbacks: bool) -> Option<Arc<Looper>> {
        // SAFETY: plain system calls; the descriptors are checked before use.
        unsafe {
            let epoll = libc::epoll_create1(libc::EPOLL_CLOEXEC);
            if epoll < 0 {
                return None;
            }
            let wake = libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK);
            if wake < 0 {
                libc::close(epoll);
                return None;
            }
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: wake as u64,
            };
            if libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, wake, &mut event) != 0 {
                libc::close(wake);
                libc::close(epoll);
                return None;
            }
            let looper = Arc::new(Looper {
                epoll,
                wake,
                allow_non_callbacks,
                state: Mutex::new(State {
                    requests: HashMap::new(),
                    pending: VecDeque::new(),
                    sequence: 0,
                }),
            });
            live().insert(Arc::as_ptr(&looper) as usize, Arc::downgrade(&looper));
            Some(looper)
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn add_fd(
        &self,
        fd: c_int,
        ident: c_int,
        events: c_int,
        request_callback: Option<Callback>,
        data: *mut c_void,
    ) -> c_int {
        if fd < 0 || fd == self.wake {
            return -1;
        }
        let ident = if request_callback.is_some() {
            POLL_CALLBACK
        } else if !self.allow_non_callbacks || ident < 0 {
            return -1;
        } else {
            ident
        };
        let mut mask = 0;
        if events as u32 & EVENT_INPUT != 0 {
            mask |= libc::EPOLLIN as u32;
        }
        if events as u32 & EVENT_OUTPUT != 0 {
            mask |= libc::EPOLLOUT as u32;
        }
        let mut state = self.state();
        state.sequence += 1;
        let request = Request {
            ident,
            callback: request_callback,
            data,
            sequence: state.sequence,
        };
        let mut event = libc::epoll_event {
            events: mask,
            u64: fd as u64,
        };
        let operation = if state.requests.contains_key(&fd) {
            libc::EPOLL_CTL_MOD
        } else {
            libc::EPOLL_CTL_ADD
        };
        // SAFETY: a plain system call on this looper's epoll set.
        if unsafe { libc::epoll_ctl(self.epoll, operation, fd, &mut event) } != 0 {
            return -1;
        }
        state.requests.insert(fd, request);
        1
    }

    fn remove_fd(&self, fd: c_int, only: Option<u64>) -> c_int {
        let mut state = self.state();
        match state.requests.get(&fd) {
            Some(request) if only.is_none_or(|sequence| sequence == request.sequence) => {}
            _ => return 0,
        }
        state.requests.remove(&fd);
        state.pending.retain(|response| response.fd != fd);
        // SAFETY: a plain system call; the fd may already be closed, which only fails it.
        unsafe { libc::epoll_ctl(self.epoll, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        1
    }

    fn wake(&self) {
        let one: u64 = 1;
        // SAFETY: writes eight bytes from a local to this looper's eventfd.
        unsafe { libc::write(self.wake, std::ptr::from_ref(&one).cast(), 8) };
    }

    /// Waits once, queues what is ready, and runs the callbacks, without holding the lock.
    fn poll_inner(&self, timeout: c_int) -> c_int {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; MAX_EVENTS];
        // SAFETY: the buffer holds MAX_EVENTS entries.
        let count = unsafe {
            libc::epoll_wait(
                self.epoll,
                events.as_mut_ptr(),
                MAX_EVENTS as c_int,
                timeout,
            )
        };
        if count < 0 {
            return if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                POLL_WAKE
            } else {
                POLL_ERROR
            };
        }
        if count == 0 {
            return POLL_TIMEOUT;
        }
        let mut result = POLL_WAKE;
        let mut callbacks = Vec::new();
        {
            let mut state = self.state();
            for event in &events[..count as usize] {
                let fd = event.u64 as c_int;
                if fd == self.wake {
                    let mut drained: u64 = 0;
                    // SAFETY: reads eight bytes into a local from this looper's eventfd.
                    unsafe { libc::read(self.wake, std::ptr::from_mut(&mut drained).cast(), 8) };
                    continue;
                }
                let Some(&request) = state.requests.get(&fd) else {
                    continue;
                };
                let mut mask = 0;
                for (epoll, ours) in [
                    (libc::EPOLLIN, EVENT_INPUT),
                    (libc::EPOLLOUT, EVENT_OUTPUT),
                    (libc::EPOLLERR, EVENT_ERROR),
                    (libc::EPOLLHUP, EVENT_HANGUP),
                ] {
                    if event.events & epoll as u32 != 0 {
                        mask |= ours;
                    }
                }
                let response = Response {
                    fd,
                    events: mask,
                    request,
                };
                if request.callback.is_some() {
                    callbacks.push(response);
                } else {
                    state.pending.push_back(response);
                }
            }
        }
        for response in callbacks {
            let Some(callback) = response.request.callback else {
                continue;
            };
            // SAFETY: the app registered this callback with ALooper_callbackFunc's signature.
            let keep =
                unsafe { callback(response.fd, response.events as c_int, response.request.data) };
            if keep == 0 {
                self.remove_fd(response.fd, Some(response.request.sequence));
            }
            result = POLL_CALLBACK;
        }
        result
    }

    fn poll_once(
        &self,
        timeout: c_int,
        out_fd: *mut c_int,
        out_events: *mut c_int,
        out_data: *mut *mut c_void,
    ) -> c_int {
        let mut result = 0;
        loop {
            if let Some(response) = self.state().pending.pop_front() {
                // SAFETY: each output pointer is null or writable, by the NDK contract.
                unsafe {
                    if !out_fd.is_null() {
                        *out_fd = response.fd;
                    }
                    if !out_events.is_null() {
                        *out_events = response.events as c_int;
                    }
                    if !out_data.is_null() {
                        *out_data = response.request.data;
                    }
                }
                return response.request.ident;
            }
            if result != 0 {
                // SAFETY: as above.
                unsafe {
                    if !out_fd.is_null() {
                        *out_fd = 0;
                    }
                    if !out_events.is_null() {
                        *out_events = 0;
                    }
                    if !out_data.is_null() {
                        *out_data = std::ptr::null_mut();
                    }
                }
                return result;
            }
            result = self.poll_inner(timeout);
        }
    }
}

thread_local! {
    static CURRENT: std::cell::RefCell<Option<Arc<Looper>>> = const { std::cell::RefCell::new(None) };
}

fn current() -> Option<Arc<Looper>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// The listener's native: runs whatever is ready on a Java Looper's thread, then keeps watching.
unsafe extern "system" fn dispatch(_env: *mut JNIEnv, _class: jclass, looper: jlong) -> jint {
    let looper = looper as *const Looper;
    // SAFETY: the listener holds a strong count of this looper, taken when it was registered and
    // never released, so the pointer stays valid as long as the listener can run.
    let looper = unsafe { &*looper };
    looper.poll_inner(0);
    JAVA_EVENT_INPUT
}

/// Joins `looper` to the calling thread's Java Looper, if it has one.
fn join_java_looper(jni: Jni, looper: &Arc<Looper>) -> Result<bool, Thrown> {
    static NATIVES: Once = Once::new();
    let mut registered = Ok(());
    NATIVES.call_once(|| {
        // SAFETY: `dispatch` matches the declared `static native int dispatch(long)`.
        registered = unsafe {
            jni.register(
                c"dev/uxdroid/system/NdkLooperListener",
                &[(c"dispatch", c"(J)I", dispatch as *mut c_void)],
            )
        };
    });
    registered?;
    let class = jni.find_class(c"android/os/Looper")?;
    // SAFETY: the framework's own signatures, on live references of this thread.
    unsafe {
        let java = jni
            .call_static(class, c"myLooper", c"()Landroid/os/Looper;", &[])?
            .l;
        jni.delete_local(class);
        if java.is_null() {
            return Ok(false);
        }
        let queue = jni
            .call(java, c"getQueue", c"()Landroid/os/MessageQueue;", &[])?
            .l;
        jni.delete_local(java);
        let descriptor = jni.new_object(c"java/io/FileDescriptor", c"()V", &[])?;
        jni.set_int_field(descriptor, c"descriptor", looper.epoll)?;
        // The listener's count is never released: a Java Looper lives as long as its thread.
        let raw = Arc::into_raw(Arc::clone(looper));
        let listener = jni.new_object(
            c"dev/uxdroid/system/NdkLooperListener",
            c"(J)V",
            &[jvalue { j: raw as jlong }],
        );
        let listener = match listener {
            Ok(listener) => listener,
            Err(thrown) => {
                drop(Arc::from_raw(raw));
                return Err(thrown);
            }
        };
        let args = [
            jvalue { l: descriptor },
            jvalue {
                i: JAVA_EVENT_INPUT,
            },
            jvalue { l: listener },
        ];
        jni.call(
            queue,
            c"addOnFileDescriptorEventListener",
            c"(Ljava/io/FileDescriptor;ILandroid/os/MessageQueue$OnFileDescriptorEventListener;)V",
            &args,
        )?;
        for local in [queue, descriptor, listener] {
            jni.delete_local(local);
        }
    }
    Ok(true)
}

/// `ALooper* ALooper_forThread(void)`: the calling thread's looper, made for a Java Looper's thread.
pub unsafe extern "C" fn for_thread() -> *mut c_void {
    if let Some(looper) = current() {
        return Arc::as_ptr(&looper).cast_mut().cast();
    }
    let Some(jni) = Jni::attached() else {
        return std::ptr::null_mut();
    };
    let Some(looper) = Looper::new(false) else {
        return std::ptr::null_mut();
    };
    match join_java_looper(jni, &looper) {
        Ok(true) => {
            let pointer = Arc::as_ptr(&looper).cast_mut().cast();
            CURRENT.with(|current| *current.borrow_mut() = Some(looper));
            pointer
        }
        Ok(false) => std::ptr::null_mut(),
        Err(Thrown) => {
            eprintln!("uxdroid: ALooper_forThread could not join this thread's Java Looper");
            std::ptr::null_mut()
        }
    }
}

/// `ALooper* ALooper_prepare(int opts)`.
pub unsafe extern "C" fn prepare(options: c_int) -> *mut c_void {
    // SAFETY: no preconditions.
    let existing = unsafe { for_thread() };
    if !existing.is_null() {
        return existing;
    }
    let Some(looper) = Looper::new(options & PREPARE_ALLOW_NON_CALLBACKS != 0) else {
        return std::ptr::null_mut();
    };
    let pointer = Arc::as_ptr(&looper).cast_mut().cast();
    CURRENT.with(|current| *current.borrow_mut() = Some(looper));
    pointer
}

/// A looper app code passed back, with a count held for the caller, or `None` if no such looper
/// is alive.
fn looper(pointer: *mut c_void) -> Option<Arc<Looper>> {
    live().get(&(pointer as usize))?.upgrade()
}

pub unsafe extern "C" fn acquire(pointer: *mut c_void) {
    // The new count outlives this call: app code gives it back through ALooper_release.
    if let Some(looper) = looper(pointer) {
        std::mem::forget(looper);
    }
}

pub unsafe extern "C" fn release(pointer: *mut c_void) {
    if let Some(looper) = looper(pointer) {
        drop(looper);
        // SAFETY: the NDK pairs each release with an acquire, whose count this gives back.
        unsafe { Arc::decrement_strong_count(pointer.cast::<Looper>()) };
    }
}

/// `int ALooper_addFd(ALooper*, int fd, int ident, int events, ALooper_callbackFunc, void* data)`.
pub unsafe extern "C" fn add_fd(
    pointer: *mut c_void,
    fd: c_int,
    ident: c_int,
    events: c_int,
    callback: Option<Callback>,
    data: *mut c_void,
) -> c_int {
    looper(pointer).map_or(-1, |looper| {
        looper.add_fd(fd, ident, events, callback, data)
    })
}

pub unsafe extern "C" fn remove_fd(pointer: *mut c_void, fd: c_int) -> c_int {
    looper(pointer).map_or(-1, |looper| looper.remove_fd(fd, None))
}

pub unsafe extern "C" fn wake(pointer: *mut c_void) {
    if let Some(looper) = looper(pointer) {
        looper.wake();
    }
}

/// `int ALooper_pollOnce(int timeoutMillis, int* outFd, int* outEvents, void** outData)`.
pub unsafe extern "C" fn poll_once(
    timeout: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    match current() {
        Some(looper) => looper.poll_once(timeout, out_fd, out_events, out_data),
        None => POLL_ERROR,
    }
}

/// `int ALooper_pollAll(...)`: polls until something other than callbacks happens or time is up.
pub unsafe extern "C" fn poll_all(
    timeout: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    let Some(looper) = current() else {
        return POLL_ERROR;
    };
    let deadline = (timeout > 0).then(|| Instant::now() + Duration::from_millis(timeout as u64));
    let mut remaining = timeout;
    loop {
        let result = looper.poll_once(remaining, out_fd, out_events, out_data);
        if result != POLL_CALLBACK {
            return result;
        }
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return POLL_TIMEOUT;
            }
            remaining = left.as_millis().clamp(1, c_int::MAX as u128) as c_int;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    static FIRED: AtomicI32 = AtomicI32::new(0);

    unsafe extern "C" fn once(fd: c_int, events: c_int, data: *mut c_void) -> c_int {
        let mut byte = 0u8;
        // SAFETY: reads one byte into a local from the test's pipe.
        unsafe { libc::read(fd, std::ptr::from_mut(&mut byte).cast(), 1) };
        assert_eq!(events as u32 & EVENT_INPUT, EVENT_INPUT);
        FIRED.fetch_add(data as i32, Ordering::Relaxed);
        0
    }

    fn pipe() -> [c_int; 2] {
        let mut fds = [0; 2];
        // SAFETY: the array holds two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        fds
    }

    #[test]
    fn a_ready_fd_runs_its_callback_once_and_then_unregisters() {
        std::thread::spawn(|| {
            // SAFETY: all pointers are this module's, and the pipe is the test's own.
            unsafe {
                let looper = prepare(0);
                assert!(!looper.is_null());
                assert_eq!(for_thread(), looper);
                let [read, write] = pipe();
                assert_eq!(
                    add_fd(
                        looper,
                        read,
                        0,
                        EVENT_INPUT as c_int,
                        Some(once),
                        3 as *mut c_void
                    ),
                    1
                );
                assert_eq!(
                    poll_once(
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    POLL_TIMEOUT
                );
                libc::write(write, b"x".as_ptr().cast(), 1);
                assert_eq!(
                    poll_once(
                        1000,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    POLL_CALLBACK
                );
                assert_eq!(FIRED.load(Ordering::Relaxed), 3);
                // The callback returned 0, so the fd is no longer watched.
                assert_eq!(remove_fd(looper, read), 0);
                libc::close(read);
                libc::close(write);
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn identifiers_come_back_with_their_data_and_wake_interrupts() {
        std::thread::spawn(|| {
            // SAFETY: as above.
            unsafe {
                let looper = prepare(PREPARE_ALLOW_NON_CALLBACKS);
                let [read, write] = pipe();
                assert_eq!(
                    add_fd(
                        looper,
                        read,
                        7,
                        EVENT_INPUT as c_int,
                        None,
                        9 as *mut c_void
                    ),
                    1
                );
                libc::write(write, b"x".as_ptr().cast(), 1);
                let (mut fd, mut events, mut data) = (0, 0, std::ptr::null_mut());
                assert_eq!(poll_all(1000, &mut fd, &mut events, &mut data), 7);
                assert_eq!((fd, events as u32, data as usize), (read, EVENT_INPUT, 9));
                assert_eq!(remove_fd(looper, read), 1);
                wake(looper);
                assert_eq!(poll_once(1000, &mut fd, &mut events, &mut data), POLL_WAKE);
                libc::close(read);
                libc::close(write);
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_looper_without_identifiers_refuses_them() {
        std::thread::spawn(|| {
            // SAFETY: as above.
            unsafe {
                let looper = prepare(0);
                let [read, write] = pipe();
                assert_eq!(
                    add_fd(
                        looper,
                        read,
                        7,
                        EVENT_INPUT as c_int,
                        None,
                        std::ptr::null_mut()
                    ),
                    -1
                );
                assert_eq!(
                    poll_once(
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    POLL_TIMEOUT
                );
                libc::close(read);
                libc::close(write);
            }
        })
        .join()
        .unwrap();
        // A thread without a looper polls nothing.
        // SAFETY: no pointers involved.
        assert_eq!(
            unsafe {
                poll_once(
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            POLL_ERROR
        );
    }
}
