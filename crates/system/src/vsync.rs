//! Frame pacing for `Choreographer`: the natives of AOSP's `DisplayEventReceiver`.
//!
//! On Android, SurfaceFlinger sends vsync events down a socket that the receiver's Looper watches.
//! Here each receiver owns a timer fd on the same Looper, through the framework's own
//! `MessageQueue.addOnFileDescriptorEventListener`. Scheduling a vsync arms the timer for the next
//! tick of a 60 Hz clock on `CLOCK_MONOTONIC`, the clock of `System.nanoTime`, and the tick calls
//! the receiver's `dispatchVsync` on its Looper thread, as Android's native receiver does.
//!
//! The timer follows the compositor: each frame a window presents asks for a Wayland frame
//! callback, and while one is pending, a scheduled vsync waits for it and fires when it comes, so
//! the app draws at the pace the compositor shows frames, but never faster than the 60 Hz the
//! display reports. The 60 Hz clock alone paces the app before any window exists, and a vsync that
//! waits for a callback fires anyway after three frames, since a compositor stops sending them for
//! hidden windows and the app's animations and lifecycle work must not stall. The NDK's
//! `AChoreographer` reaches the same clock through [`clock`].

use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jni_sys::{JNIEnv, jclass, jint, jlong, jobject, jvalue};

use crate::handles::Handles;
use crate::java::{self, Field, Pending};
use crate::{Method, register, throw};

/// 60 Hz, the refresh rate `DisplayService` reports.
pub const FRAME_INTERVAL_NANOS: i64 = 1_000_000_000 / 60;

/// `MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT`.
const EVENT_INPUT: jint = 1;

/// The tick at or after which a vsync scheduled at `now` fires.
pub fn next_tick(now: i64) -> i64 {
    (now / FRAME_INTERVAL_NANOS + 1) * FRAME_INTERVAL_NANOS
}

/// How long a vsync waits for the compositor's frame callback before it fires anyway: a hidden
/// window gets none, and its app must not stall.
pub const FALLBACK_NANOS: i64 = 3 * FRAME_INTERVAL_NANOS;

/// When a vsync scheduled at `now` fires, `last` being the last vsync delivered: on the next tick
/// of the 60 Hz clock, or when a window waits for the compositor's frame callback, at that
/// callback, with this deadline in case it never comes. Never sooner than one period after `last`.
pub fn deadline(now: i64, last: i64, waiting_for_frame: bool) -> i64 {
    if waiting_for_frame {
        on_frame(now, last) + FALLBACK_NANOS
    } else {
        next_tick(now).max(last.saturating_add(FRAME_INTERVAL_NANOS))
    }
}

/// When a vsync waiting for a frame callback fires once the callback came at `now`: at once, but
/// never sooner after the last vsync than the refresh period the display reports.
pub fn on_frame(now: i64, last: i64) -> i64 {
    now.max(last.saturating_add(FRAME_INTERVAL_NANOS))
}

/// Windows that asked the compositor for a frame callback and have not had it yet.
static FRAMES_PENDING: AtomicUsize = AtomicUsize::new(0);
/// When the last vsync was delivered, to any receiver.
static LAST_VSYNC: AtomicI64 = AtomicI64::new(0);
/// An armed vsync timer, whether it waits for the compositor's frame, and where it reports its
/// tick, which its owner keeps alive until it disarms the timer.
struct Armed {
    fd: RawFd,
    tick: *const AtomicI64,
    waiting: bool,
}

// SAFETY: the tick is an atomic its owner keeps alive while the entry exists; it is only read and
// written under ARMED's lock.
unsafe impl Send for Armed {}

static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

fn armed() -> std::sync::MutexGuard<'static, Vec<Armed>> {
    ARMED.lock().unwrap_or_else(|p| p.into_inner())
}

fn set_timer(fd: RawFd, when: i64) -> std::io::Result<()> {
    let when = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: when / 1_000_000_000,
            tv_nsec: when % 1_000_000_000,
        },
    };
    // SAFETY: `when` is a valid itimerspec; an fd that is no timer fails with an error.
    let status =
        unsafe { libc::timerfd_settime(fd, libc::TFD_TIMER_ABSTIME, &when, std::ptr::null_mut()) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Arms the timer `fd` for the next vsync and stores in `tick` the time it will report: the
/// compositor's next frame callback when a window waits for one, else the 60 Hz clock. `tick` must
/// live until the timer fires or [`disarm`] is called.
pub fn arm(fd: RawFd, tick: &AtomicI64) -> std::io::Result<()> {
    let now = monotonic_nanos();
    let mut armed = armed();
    let waiting = FRAMES_PENDING.load(Ordering::Acquire) > 0;
    let when = deadline(now, LAST_VSYNC.load(Ordering::Acquire), waiting);
    tick.store(when, Ordering::Release);
    armed.retain(|entry| entry.fd != fd);
    set_timer(fd, when)?;
    armed.push(Armed { fd, tick, waiting });
    Ok(())
}

/// Forgets the timer `fd`: it fired, or it is about to close.
pub fn disarm(fd: RawFd) {
    armed().retain(|entry| entry.fd != fd);
}

/// A window asked the compositor for a frame callback. Vsyncs already armed on the 60 Hz clock,
/// such as the one an animation asked for before the frame was drawn, wait for it instead.
pub fn frame_requested() {
    FRAMES_PENDING.fetch_add(1, Ordering::AcqRel);
    let now = monotonic_nanos();
    let when = deadline(now, LAST_VSYNC.load(Ordering::Acquire), true);
    for entry in armed().iter_mut().filter(|entry| !entry.waiting) {
        // SAFETY: the owner keeps the tick alive until it disarms, which waits for this lock.
        let tick = unsafe { &*entry.tick };
        // One that is already due fires as it is.
        if tick.load(Ordering::Acquire) <= now {
            continue;
        }
        entry.waiting = true;
        tick.store(when, Ordering::Release);
        if let Err(error) = set_timer(entry.fd, when) {
            eprintln!("uxdroid: cannot move a vsync timer: {error}");
        }
    }
}

/// A window's frame callback will never come: its surface is gone.
pub fn frame_dropped() {
    let _ = FRAMES_PENDING.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
}

/// The compositor answered a window's frame callback: every vsync waiting for one fires now.
pub fn frame_done() {
    frame_dropped();
    let now = monotonic_nanos();
    let when = on_frame(now, LAST_VSYNC.load(Ordering::Acquire));
    for entry in armed().iter_mut().filter(|entry| entry.waiting) {
        entry.waiting = false;
        // SAFETY: the owner keeps the tick alive until it disarms, which waits for this lock.
        unsafe { (*entry.tick).store(when, Ordering::Release) };
        if let Err(error) = set_timer(entry.fd, when) {
            eprintln!("uxdroid: cannot fire a vsync timer: {error}");
        }
    }
}

/// A vsync was delivered at `tick`.
pub fn delivered(tick: i64) {
    LAST_VSYNC.fetch_max(tick, Ordering::AcqRel);
}

/// The frame clock as the NDK's `AChoreographer`, in another library, reaches it: by the address
/// `VsyncListener.clock()` returns. These are host functions between uxdroid's own libraries, not
/// exports to app code.
#[repr(C)]
pub struct Clock {
    pub version: u32,
    /// [`arm`]: 0, or an errno.
    pub arm: unsafe extern "C" fn(fd: i32, tick: *const AtomicI64) -> i32,
    pub disarm: extern "C" fn(fd: i32),
    pub delivered: extern "C" fn(tick: i64),
}

unsafe extern "C" fn clock_arm(fd: i32, tick: *const AtomicI64) -> i32 {
    if tick.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: the caller keeps `tick` alive until it disarms the timer, as `Clock` documents.
    match arm(fd, unsafe { &*tick }) {
        Ok(()) => 0,
        Err(error) => error.raw_os_error().unwrap_or(libc::EIO),
    }
}

extern "C" fn clock_disarm(fd: i32) {
    disarm(fd);
}

extern "C" fn clock_delivered(tick: i64) {
    delivered(tick);
}

static CLOCK: Clock = Clock {
    version: 1,
    arm: clock_arm,
    disarm: clock_disarm,
    delivered: clock_delivered,
};

/// `VsyncListener.clock()`.
unsafe extern "system" fn native_clock(_env: *mut JNIEnv, _class: jclass) -> jlong {
    &CLOCK as *const Clock as jlong
}

struct Receiver {
    timer: OwnedFd,
    /// WeakReference<DisplayEventReceiver>, WeakReference<VsyncEventData>, the MessageQueue and
    /// the FileDescriptor of the timer: global references.
    receiver: jobject,
    data: jobject,
    queue: jobject,
    descriptor: jobject,
    armed: AtomicBool,
    tick: AtomicI64,
    frame: AtomicI64,
}

// SAFETY: the fd may be used from any thread, and the Java references are global.
unsafe impl Send for Receiver {}
// SAFETY: as above; the mutable state is atomic.
unsafe impl Sync for Receiver {}

impl Drop for Receiver {
    fn drop(&mut self) {
        disarm(self.timer.as_raw_fd());
        // The Looper must stop watching the fd before it closes. With an exception pending, the
        // listener was never added, and JNI allows no calls but deleting references.
        if let Some(env) = java::attached_env()
            // SAFETY: an attached thread's environment.
            && !unsafe { ((**env).v1_2.ExceptionCheck)(env) }
        {
            let args = [jvalue { l: self.descriptor }];
            // SAFETY: an attached thread's environment and this receiver's own references.
            let removed = unsafe {
                java::call_void(
                    env,
                    self.queue,
                    c"removeOnFileDescriptorEventListener",
                    c"(Ljava/io/FileDescriptor;)V",
                    &args,
                )
            };
            if removed.is_err() {
                // SAFETY: as above.
                unsafe { ((**env).v1_1.ExceptionClear)(env) };
            }
        }
        // SAFETY: these are this receiver's own global references.
        unsafe { java::delete_globals(&[self.receiver, self.data, self.queue, self.descriptor]) };
    }
}

static RECEIVERS: Handles<Arc<Receiver>> = Handles::new();

/// `dev.uxdroid.system.VsyncListener`, found when the natives are registered.
static LISTENER_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn monotonic_nanos() -> i64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec to write.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec * 1_000_000_000 + now.tv_nsec
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn create(
    env: *mut JNIEnv,
    receiver: jobject,
    data: jobject,
    queue: jobject,
) -> Result<jlong, Pending> {
    // SAFETY: plain system call; the result is checked.
    let fd = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: as above.
        unsafe {
            throw(
                env,
                c"java/lang/RuntimeException",
                &format!("no vsync timer: {error}"),
            )
        };
        return Err(Pending);
    }
    // SAFETY: timerfd_create just returned this descriptor, and nothing else owns it.
    let timer = unsafe { OwnedFd::from_raw_fd(fd) };
    let listener_class = LISTENER_CLASS.load(Ordering::Acquire) as jclass;
    // SAFETY: as above; the class reference is global and the constructors match.
    unsafe {
        let descriptor_class = java::find_class(env, c"java/io/FileDescriptor")?;
        let descriptor = java::new_object(env, descriptor_class, c"()V", &[])?;
        java::delete_local(env, descriptor_class);
        java::set_field(env, descriptor, c"descriptor", Field::Int(fd))?;
        let entry = Arc::new(Receiver {
            timer,
            receiver: java::keep(env, receiver),
            data: java::keep(env, data),
            queue: java::keep(env, queue),
            descriptor: java::global(env, descriptor),
            armed: AtomicBool::new(false),
            tick: AtomicI64::new(0),
            frame: AtomicI64::new(0),
        });
        let (queue, descriptor) = (entry.queue, entry.descriptor);
        let id = RECEIVERS.add(entry);
        // The queue keeps the listener, which knows the receiver only by its id.
        let registered = java::new_object(env, listener_class, c"(J)V", &[jvalue { j: id }])
            .and_then(|listener| {
                let args = [
                    jvalue { l: descriptor },
                    jvalue { i: EVENT_INPUT },
                    jvalue { l: listener },
                ];
                let added = java::call_void(
                    env,
                    queue,
                    c"addOnFileDescriptorEventListener",
                    c"(Ljava/io/FileDescriptor;ILandroid/os/MessageQueue$OnFileDescriptorEventListener;)V",
                    &args,
                );
                java::delete_local(env, listener);
                added
            });
        match registered {
            Ok(()) => Ok(id),
            Err(Pending) => {
                drop(RECEIVERS.remove(id));
                Err(Pending)
            }
        }
    }
}

unsafe extern "system" fn native_init(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jobject,
    data: jobject,
    queue: jobject,
    _vsync_source: jint,
    _event_registration: jint,
    _layer_handle: jlong,
) -> jlong {
    // SAFETY: ART passes a valid environment and the receiver's references.
    unsafe { create(env, receiver, data, queue) }.unwrap_or(0)
}

/// What `NativeAllocationRegistry` calls when a receiver is disposed.
extern "C" fn native_free(receiver: *mut c_void) {
    drop(RECEIVERS.remove(receiver as i64));
}

unsafe extern "system" fn native_finalizer(_env: *mut JNIEnv, _class: jclass) -> jlong {
    native_free as *const () as jlong
}

unsafe extern "system" fn native_schedule(env: *mut JNIEnv, _class: jclass, receiver: jlong) {
    let Some(receiver) = RECEIVERS.get(receiver) else {
        // SAFETY: ART passes a valid environment.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalStateException",
                "the receiver was disposed",
            )
        };
        return;
    };
    if receiver.armed.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(error) = arm(receiver.timer.as_raw_fd(), &receiver.tick) {
        receiver.armed.store(false, Ordering::Release);
        // SAFETY: ART passes a valid environment.
        unsafe {
            throw(
                env,
                c"java/lang/RuntimeException",
                &format!("cannot arm the vsync timer: {error}"),
            )
        };
    }
}

/// Fills a VsyncEventData with one frame timeline for the vsync at `tick`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `data` a VsyncEventData.
unsafe fn fill(env: *mut JNIEnv, data: jobject, tick: i64, frame: i64) -> Result<(), Pending> {
    // SAFETY: as above.
    unsafe {
        java::set_field(
            env,
            data,
            c"frameInterval",
            Field::Long(FRAME_INTERVAL_NANOS),
        )?;
        java::set_field(env, data, c"preferredFrameTimelineIndex", Field::Int(0))?;
        java::set_field(env, data, c"frameTimelinesLength", Field::Int(1))?;
        let timelines = java::object_field(
            env,
            data,
            c"frameTimelines",
            c"[Landroid/view/DisplayEventReceiver$VsyncEventData$FrameTimeline;",
        )?;
        if timelines.is_null() {
            return Ok(());
        }
        let first = ((**env).v1_1.GetObjectArrayElement)(env, timelines, 0);
        java::delete_local(env, timelines);
        if first.is_null() {
            return Ok(());
        }
        let next = tick + FRAME_INTERVAL_NANOS;
        java::set_field(env, first, c"vsyncId", Field::Long(frame))?;
        java::set_field(env, first, c"expectedPresentationTime", Field::Long(next))?;
        java::set_field(env, first, c"deadline", Field::Long(next))?;
        java::delete_local(env, first);
    }
    Ok(())
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn dispatch(env: *mut JNIEnv, receiver: &Receiver) -> Result<jint, Pending> {
    let mut expirations = 0u64;
    // SAFETY: reads 8 bytes into `expirations` from this receiver's own non-blocking timer.
    let read = unsafe {
        libc::read(
            receiver.timer.as_raw_fd(),
            (&mut expirations as *mut u64).cast(),
            8,
        )
    };
    if read != 8 {
        return Ok(EVENT_INPUT);
    }
    receiver.armed.store(false, Ordering::Release);
    disarm(receiver.timer.as_raw_fd());
    let tick = receiver.tick.load(Ordering::Acquire);
    delivered(tick);
    let frame = receiver.frame.fetch_add(1, Ordering::AcqRel) + 1;
    // SAFETY: as above; both are WeakReferences.
    unsafe {
        let target =
            java::call_object(env, receiver.receiver, c"get", c"()Ljava/lang/Object;", &[])?;
        if target.is_null() {
            // The receiver is gone: stop watching the timer.
            return Ok(0);
        }
        let data = java::call_object(env, receiver.data, c"get", c"()Ljava/lang/Object;", &[])?;
        if !data.is_null() {
            fill(env, data, tick, frame)?;
            java::delete_local(env, data);
        }
        let args = [
            jvalue { j: tick },
            jvalue { j: 0 },
            jvalue { i: frame as jint },
        ];
        java::call_void(env, target, c"dispatchVsync", c"(JJI)V", &args)?;
        java::delete_local(env, target);
    }
    Ok(EVENT_INPUT)
}

/// `VsyncListener.dispatch(long)`: the timer fired on the receiver's Looper thread.
unsafe extern "system" fn listener_dispatch(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jlong,
) -> jint {
    let Some(receiver) = RECEIVERS.get(receiver) else {
        return 0;
    };
    // SAFETY: ART passes a valid environment. A pending exception reaches the Looper's caller.
    unsafe { dispatch(env, &receiver) }.unwrap_or(0)
}

unsafe extern "system" fn native_latest(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jlong,
) -> jobject {
    let Some(receiver) = RECEIVERS.get(receiver) else {
        return std::ptr::null_mut();
    };
    let tick = receiver.tick.load(Ordering::Acquire);
    let frame = receiver.frame.load(Ordering::Acquire);
    // SAFETY: ART passes a valid environment; the constructor makes the frame timelines.
    let made = unsafe {
        java::find_class(env, c"android/view/DisplayEventReceiver$VsyncEventData").and_then(
            |class| {
                let data = java::new_object(env, class, c"()V", &[])?;
                fill(env, data, tick, frame)?;
                Ok(data)
            },
        )
    };
    made.unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let receiver: [Method; 4] = [
        (
            c"nativeInit",
            c"(Ljava/lang/ref/WeakReference;Ljava/lang/ref/WeakReference;Landroid/os/MessageQueue;IIJ)J",
            f!(native_init),
        ),
        (
            c"nativeGetDisplayEventReceiverFinalizer",
            c"()J",
            f!(native_finalizer),
        ),
        (c"nativeScheduleVsync", c"(J)V", f!(native_schedule)),
        (
            c"nativeGetLatestVsyncEventData",
            c"(J)Landroid/view/DisplayEventReceiver$VsyncEventData;",
            f!(native_latest),
        ),
    ];
    let listener: [Method; 2] = [
        (c"dispatch", c"(J)I", f!(listener_dispatch)),
        (c"clock", c"()J", f!(native_clock)),
    ];
    // SAFETY: each table matches its Java declarations.
    unsafe {
        register(env, c"android/view/DisplayEventReceiver", &receiver)?;
        register(env, c"dev/uxdroid/system/VsyncListener", &listener)?;
        let class = java::find_class(env, c"dev/uxdroid/system/VsyncListener")
            .map_err(|Pending| "class dev/uxdroid/system/VsyncListener not found".to_string())?;
        LISTENER_CLASS.store(java::global(env, class).cast(), Ordering::Release);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vsync_waits_for_the_compositor_frame_with_a_deadline() {
        let now = 10 * FRAME_INTERVAL_NANOS + 5;
        // No window waits for a frame: the 60 Hz clock.
        assert_eq!(deadline(now, 0, false), next_tick(now));
        // Back on the clock after a frame callback fired off it: never sooner than one period.
        let last = next_tick(now) - FRAME_INTERVAL_NANOS / 4;
        assert_eq!(deadline(now, last, false), last + FRAME_INTERVAL_NANOS);
        // A frame is pending: the callback will fire it; this is only the deadline.
        assert_eq!(deadline(now, 0, true), now + FALLBACK_NANOS);
        // Never earlier than the reported rate allows after the last vsync.
        let last = now - 1;
        assert_eq!(
            deadline(now, last, true),
            last + FRAME_INTERVAL_NANOS + FALLBACK_NANOS
        );
    }

    #[test]
    fn a_frame_callback_fires_the_vsync_at_once_within_the_reported_rate() {
        let last = 100 * FRAME_INTERVAL_NANOS;
        // The compositor's frame came late (30 Hz): at once.
        let late = last + 2 * FRAME_INTERVAL_NANOS;
        assert_eq!(on_frame(late, last), late);
        // It came early (a fast or eager compositor): not before 60 Hz allows.
        assert_eq!(on_frame(last + 1000, last), last + FRAME_INTERVAL_NANOS);
    }

    #[test]
    fn a_timer_that_cannot_be_armed_leaves_no_entry() {
        // A pipe is no timer: arming it fails, and no later frame event may set it.
        let mut fds = [0; 2];
        // SAFETY: a valid array of two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let tick = AtomicI64::new(0);
        assert!(arm(fds[0], &tick).is_err());
        assert!(!armed().iter().any(|entry| entry.fd == fds[0]));
        // SAFETY: the pipe's own descriptors, closed once.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn vsync_fires_on_the_next_60_hz_tick() {
        assert_eq!(next_tick(0), FRAME_INTERVAL_NANOS);
        assert_eq!(next_tick(FRAME_INTERVAL_NANOS - 1), FRAME_INTERVAL_NANOS);
        // A request exactly on a tick waits for the next one, never fires in the past.
        assert_eq!(next_tick(FRAME_INTERVAL_NANOS), 2 * FRAME_INTERVAL_NANOS);
        let now = 1_234_567_890_123;
        let tick = next_tick(now);
        assert!(tick > now && tick - now <= FRAME_INTERVAL_NANOS);
        assert_eq!(tick % FRAME_INTERVAL_NANOS, 0);
    }
}
