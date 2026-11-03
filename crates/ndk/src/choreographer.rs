//! `AChoreographer`: frame callbacks for native code, as Flutter's engine asks for them.
//!
//! Each thread with an `ALooper` gets one choreographer, a timer on that looper. A posted callback
//! arms the timer for the next vsync of the same clock the framework's `Choreographer` follows, in
//! `libuxdroid_system.so`: the compositor's frame callbacks while a window waits for one, else a
//! 60 Hz clock. That library is reached through `dev.uxdroid.system.VsyncListener.clock()`; without
//! it, such as under milestone 3's platform, the timer follows a 60 Hz clock of its own. Callbacks
//! run on the looper's thread, as on Android, and a callback posted from one may post the next.
//!
//! The choreographer of a thread lives as long as the process, as Android's lives as long as its
//! thread.

use std::cell::Cell;
use std::ffi::{c_int, c_long, c_void};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::java::Jni;
use crate::looper;

/// 60 Hz, the refresh rate `DisplayService` reports.
pub const FRAME_INTERVAL_NANOS: i64 = 1_000_000_000 / 60;
const POLL_CALLBACK: c_int = -2;
const EVENT_INPUT: c_int = 1;

type FrameCallback = unsafe extern "C" fn(c_long, *mut c_void);
type FrameCallback64 = unsafe extern "C" fn(i64, *mut c_void);
type VsyncCallback = unsafe extern "C" fn(*const FrameData, *mut c_void);
type RefreshRateCallback = unsafe extern "C" fn(i64, *mut c_void);

#[derive(Clone, Copy)]
enum Kind {
    Frame(FrameCallback),
    Frame64(FrameCallback64),
    Vsync(VsyncCallback),
}

#[derive(Clone, Copy)]
struct Posted {
    kind: Kind,
    data: *mut c_void,
    /// Not before this time, for the delayed variants.
    due: i64,
}

/// `AChoreographerFrameCallbackData`: one frame timeline, as the framework's receiver fills it.
#[repr(C)]
pub struct FrameData {
    frame_time: i64,
    vsync_id: i64,
    deadline: i64,
    expected_presentation: i64,
}

#[derive(Default)]
struct State {
    posted: Vec<Posted>,
    refresh: Vec<(RefreshRateCallback, *mut c_void)>,
    /// Refresh rate callbacks registered since the last vsync, which hear the rate once.
    refresh_due: Vec<(RefreshRateCallback, *mut c_void)>,
    armed: bool,
    frame: i64,
}

pub struct Choreographer {
    timer: OwnedFd,
    /// The time the armed vsync reports, which the frame clock may change until it fires.
    tick: AtomicI64,
    state: Mutex<State>,
}

// SAFETY: app pointers are only handed back to app code on the looper's thread; the rest is
// atomics, a mutex and a descriptor.
unsafe impl Send for Choreographer {}
// SAFETY: as above.
unsafe impl Sync for Choreographer {}

/// The frame clock of `libuxdroid_system.so`, laid out as its `vsync::Clock`.
#[repr(C)]
struct Clock {
    version: u32,
    arm: unsafe extern "C" fn(fd: i32, tick: *const AtomicI64) -> i32,
    disarm: extern "C" fn(fd: i32),
    delivered: extern "C" fn(tick: i64),
}

fn clock() -> Option<&'static Clock> {
    static CLOCK: OnceLock<usize> = OnceLock::new();
    let address = *CLOCK.get_or_init(|| {
        let Some(jni) = Jni::current() else {
            return 0;
        };
        let Ok(class) = jni.find_class(c"dev/uxdroid/system/VsyncListener") else {
            jni.quietly();
            return 0;
        };
        // SAFETY: a live class and a static method taking nothing and returning a long.
        let address = unsafe { jni.call_static(class, c"clock", c"()J", &[]) };
        jni.delete_local(class);
        match address {
            // SAFETY: a long was returned.
            Ok(value) => (unsafe { value.j }) as usize,
            Err(_) => {
                jni.quietly();
                0
            }
        }
    });
    // SAFETY: a non-zero address is the system library's static `Clock`, which lives as long as
    // the process; version 1 has these fields.
    (address != 0)
        .then(|| unsafe { &*(address as *const Clock) })
        .filter(|clock| clock.version >= 1)
}

fn monotonic_nanos() -> i64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec to write.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec * 1_000_000_000 + now.tv_nsec
}

fn set_timer(fd: c_int, when: i64) -> bool {
    let value = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: when / 1_000_000_000,
            tv_nsec: when % 1_000_000_000,
        },
    };
    // SAFETY: the choreographer's own timer and a valid itimerspec.
    unsafe { libc::timerfd_settime(fd, libc::TFD_TIMER_ABSTIME, &value, std::ptr::null_mut()) == 0 }
}

impl Choreographer {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Arms the timer for the next vsync, at or after the earliest due callback.
    fn schedule(&self, state: &mut State) {
        if state.armed || (state.posted.is_empty() && state.refresh_due.is_empty()) {
            return;
        }
        let now = monotonic_nanos();
        let earliest = state.posted.iter().map(|posted| posted.due).min();
        let fd = self.timer.as_raw_fd();
        state.armed = match (earliest, clock()) {
            // Delayed: wake when due, then wait for the vsync after it.
            (Some(due), _) if due > now + FRAME_INTERVAL_NANOS => {
                self.tick.store(0, Ordering::Release);
                set_timer(fd, due - FRAME_INTERVAL_NANOS)
            }
            // SAFETY: the tick lives as long as the process, as the choreographer does.
            (_, Some(clock)) => unsafe { (clock.arm)(fd, &self.tick) == 0 },
            (_, None) => {
                let tick = (now / FRAME_INTERVAL_NANOS + 1) * FRAME_INTERVAL_NANOS;
                self.tick.store(tick, Ordering::Release);
                set_timer(fd, tick)
            }
        };
    }

    fn post(&self, kind: Kind, data: *mut c_void, delay_nanos: i64) {
        let mut state = self.state();
        let due = monotonic_nanos().saturating_add(delay_nanos.max(0));
        state.posted.push(Posted { kind, data, due });
        self.schedule(&mut state);
    }

    /// The timer fired, on the looper's thread.
    fn fire(&self) {
        let mut expirations = 0u64;
        // SAFETY: reads 8 bytes into `expirations` from this choreographer's non-blocking timer.
        let read = unsafe {
            libc::read(
                self.timer.as_raw_fd(),
                (&mut expirations as *mut u64).cast(),
                8,
            )
        };
        if read != 8 {
            return;
        }
        let tick = self.tick.load(Ordering::Acquire);
        let (ready, refresh, frame) = {
            let mut state = self.state();
            state.armed = false;
            if tick == 0 {
                // The wake-up of a delayed callback: now wait for its vsync.
                self.schedule(&mut state);
                return;
            }
            if let Some(clock) = clock() {
                (clock.disarm)(self.timer.as_raw_fd());
                (clock.delivered)(tick);
            }
            state.frame += 1;
            let (ready, later): (Vec<Posted>, Vec<Posted>) =
                state.posted.iter().partition(|posted| posted.due <= tick);
            state.posted = later;
            let refresh = std::mem::take(&mut state.refresh_due);
            self.schedule(&mut state);
            (ready, refresh, state.frame)
        };
        let data = FrameData {
            frame_time: tick,
            vsync_id: frame,
            deadline: tick + FRAME_INTERVAL_NANOS,
            expected_presentation: tick + FRAME_INTERVAL_NANOS,
        };
        for (callback, user) in refresh {
            // SAFETY: the app registered this callback with AChoreographer_refreshRateCallback's
            // signature, to be called on this thread.
            unsafe { callback(FRAME_INTERVAL_NANOS, user) };
        }
        for posted in ready {
            // SAFETY: the app posted each callback with the signature its kind records.
            unsafe {
                match posted.kind {
                    Kind::Frame(callback) => callback(tick as c_long, posted.data),
                    Kind::Frame64(callback) => callback(tick, posted.data),
                    Kind::Vsync(callback) => callback(&data, posted.data),
                }
            }
        }
    }
}

/// The looper callback of every choreographer's timer.
unsafe extern "C" fn on_timer(_fd: c_int, _events: c_int, data: *mut c_void) -> c_int {
    // SAFETY: `data` is the address of a choreographer, which lives as long as the process.
    let choreographer = unsafe { &*data.cast::<Choreographer>() };
    choreographer.fire();
    1
}

thread_local! {
    static INSTANCE: Cell<*const Choreographer> = const { Cell::new(std::ptr::null()) };
}

/// `AChoreographer* AChoreographer_getInstance(void)`: the calling thread's choreographer, or null
/// when the thread has no looper.
pub unsafe extern "C" fn get_instance() -> *mut c_void {
    let known = INSTANCE.with(Cell::get);
    if !known.is_null() {
        return known.cast_mut().cast();
    }
    // SAFETY: no preconditions.
    let looper = unsafe { looper::for_thread() };
    if looper.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: plain system call; the result is checked.
    let fd = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
        )
    };
    if fd < 0 {
        return std::ptr::null_mut();
    }
    let choreographer: &'static Choreographer = Box::leak(Box::new(Choreographer {
        // SAFETY: timerfd_create just returned this descriptor, and nothing else owns it.
        timer: unsafe { OwnedFd::from_raw_fd(fd) },
        tick: AtomicI64::new(0),
        state: Mutex::new(State::default()),
    }));
    let pointer = choreographer as *const Choreographer;
    // SAFETY: a live looper of this thread, the timer, and a callback with ALooper's signature.
    let added = unsafe {
        looper::add_fd(
            looper,
            fd,
            POLL_CALLBACK,
            EVENT_INPUT,
            Some(on_timer),
            pointer.cast_mut().cast(),
        )
    };
    if added != 1 {
        return std::ptr::null_mut();
    }
    INSTANCE.with(|instance| instance.set(pointer));
    pointer.cast_mut().cast()
}

/// The choreographer app code passed back, if it is one of ours.
fn choreographer<'a>(pointer: *mut c_void) -> Option<&'a Choreographer> {
    let mine = INSTANCE.with(Cell::get);
    // A choreographer belongs to its thread, as on Android; one from another thread is refused.
    (!pointer.is_null() && std::ptr::eq(pointer.cast_const().cast(), mine))
        // SAFETY: it is this thread's, which lives as long as the process.
        .then(|| unsafe { &*mine })
}

/// `void AChoreographer_postFrameCallback(AChoreographer*, AChoreographer_frameCallback, void*)`.
pub unsafe extern "C" fn post_frame_callback(
    pointer: *mut c_void,
    callback: Option<FrameCallback>,
    data: *mut c_void,
) {
    // SAFETY: as below.
    unsafe { post_frame_callback_delayed(pointer, callback, data, 0) }
}

/// `void AChoreographer_postFrameCallbackDelayed(AChoreographer*, ..., void*, long delayMillis)`.
pub unsafe extern "C" fn post_frame_callback_delayed(
    pointer: *mut c_void,
    callback: Option<FrameCallback>,
    data: *mut c_void,
    delay_millis: c_long,
) {
    if let (Some(choreographer), Some(callback)) = (choreographer(pointer), callback) {
        choreographer.post(
            Kind::Frame(callback),
            data,
            delay_millis.saturating_mul(1_000_000),
        );
    }
}

/// `void AChoreographer_postFrameCallback64(AChoreographer*, AChoreographer_frameCallback64,
/// void*)`.
pub unsafe extern "C" fn post_frame_callback64(
    pointer: *mut c_void,
    callback: Option<FrameCallback64>,
    data: *mut c_void,
) {
    // SAFETY: as below.
    unsafe { post_frame_callback_delayed64(pointer, callback, data, 0) }
}

/// `void AChoreographer_postFrameCallbackDelayed64(AChoreographer*, ..., void*, uint32_t delay)`.
pub unsafe extern "C" fn post_frame_callback_delayed64(
    pointer: *mut c_void,
    callback: Option<FrameCallback64>,
    data: *mut c_void,
    delay_millis: u32,
) {
    if let (Some(choreographer), Some(callback)) = (choreographer(pointer), callback) {
        choreographer.post(
            Kind::Frame64(callback),
            data,
            i64::from(delay_millis) * 1_000_000,
        );
    }
}

/// `void AChoreographer_postVsyncCallback(AChoreographer*, AChoreographer_vsyncCallback, void*)`.
pub unsafe extern "C" fn post_vsync_callback(
    pointer: *mut c_void,
    callback: Option<VsyncCallback>,
    data: *mut c_void,
) {
    if let (Some(choreographer), Some(callback)) = (choreographer(pointer), callback) {
        choreographer.post(Kind::Vsync(callback), data, 0);
    }
}

/// `void AChoreographer_registerRefreshRateCallback(AChoreographer*, ..., void*)`: the callback
/// hears the display's refresh period once now, as on Android, and again if it ever changes, which
/// it does not here.
pub unsafe extern "C" fn register_refresh_rate_callback(
    pointer: *mut c_void,
    callback: Option<RefreshRateCallback>,
    data: *mut c_void,
) {
    let (Some(choreographer), Some(callback)) = (choreographer(pointer), callback) else {
        return;
    };
    let mut state = choreographer.state();
    let entry = (callback, data);
    if state
        .refresh
        .iter()
        .any(|known| known.0 as usize == callback as usize && known.1 == data)
    {
        return;
    }
    state.refresh.push(entry);
    state.refresh_due.push(entry);
    choreographer.schedule(&mut state);
}

/// `void AChoreographer_unregisterRefreshRateCallback(AChoreographer*, ..., void*)`.
pub unsafe extern "C" fn unregister_refresh_rate_callback(
    pointer: *mut c_void,
    callback: Option<RefreshRateCallback>,
    data: *mut c_void,
) {
    let (Some(choreographer), Some(callback)) = (choreographer(pointer), callback) else {
        return;
    };
    let same = |known: &(RefreshRateCallback, *mut c_void)| {
        known.0 as usize == callback as usize && known.1 == data
    };
    let mut state = choreographer.state();
    state.refresh.retain(|known| !same(known));
    state.refresh_due.retain(|known| !same(known));
}

fn frame_data<'a>(data: *const c_void) -> Option<&'a FrameData> {
    // SAFETY: app code passes back the frame data of the vsync callback it is running in.
    (!data.is_null()).then(|| unsafe { &*data.cast::<FrameData>() })
}

/// `int64_t AChoreographerFrameCallbackData_getFrameTimeNanos(const ...*)`.
pub unsafe extern "C" fn data_frame_time(data: *const c_void) -> i64 {
    frame_data(data).map_or(0, |data| data.frame_time)
}

/// `size_t AChoreographerFrameCallbackData_getFrameTimelinesLength(const ...*)`: one timeline.
pub unsafe extern "C" fn data_timelines_length(data: *const c_void) -> usize {
    usize::from(frame_data(data).is_some())
}

/// `size_t AChoreographerFrameCallbackData_getPreferredFrameTimelineIndex(const ...*)`.
pub unsafe extern "C" fn data_preferred_timeline(_data: *const c_void) -> usize {
    0
}

/// `AVsyncId AChoreographerFrameCallbackData_getFrameTimelineVsyncId(const ...*, size_t)`.
pub unsafe extern "C" fn data_vsync_id(data: *const c_void, index: usize) -> i64 {
    frame_data(data)
        .filter(|_| index == 0)
        .map_or(0, |data| data.vsync_id)
}

/// `int64_t AChoreographerFrameCallbackData_getFrameTimelineExpectedPresentationTimeNanos(...)`.
pub unsafe extern "C" fn data_expected_presentation(data: *const c_void, index: usize) -> i64 {
    frame_data(data)
        .filter(|_| index == 0)
        .map_or(0, |data| data.expected_presentation)
}

/// `int64_t AChoreographerFrameCallbackData_getFrameTimelineDeadlineNanos(const ...*, size_t)`.
pub unsafe extern "C" fn data_deadline(data: *const c_void, index: usize) -> i64 {
    frame_data(data)
        .filter(|_| index == 0)
        .map_or(0, |data| data.deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// What the callbacks saw, per test thread: (kind, frame time).
    static SEEN: StdMutex<Vec<(u64, &'static str, i64)>> = StdMutex::new(Vec::new());

    fn thread_id() -> u64 {
        // SAFETY: no preconditions.
        unsafe { libc::syscall(libc::SYS_gettid) as u64 }
    }

    fn seen(kind: &'static str, time: i64) {
        SEEN.lock().unwrap().push((thread_id(), kind, time));
    }

    fn mine() -> Vec<(&'static str, i64)> {
        let id = thread_id();
        SEEN.lock()
            .unwrap()
            .iter()
            .filter(|(thread, _, _)| *thread == id)
            .map(|(_, kind, time)| (*kind, *time))
            .collect()
    }

    unsafe extern "C" fn frame(time: c_long, _: *mut c_void) {
        seen("frame", time);
    }

    unsafe extern "C" fn frame64(time: i64, data: *mut c_void) {
        seen("frame64", time);
        // A callback that posts the next frame's, as an animation does.
        if !data.is_null() {
            // SAFETY: `data` is this thread's choreographer.
            unsafe { post_frame_callback64(data, Some(frame64_last), std::ptr::null_mut()) };
        }
    }

    unsafe extern "C" fn frame64_last(time: i64, _: *mut c_void) {
        seen("next", time);
    }

    unsafe extern "C" fn vsync(data: *const FrameData, _: *mut c_void) {
        let data = data.cast::<c_void>();
        // SAFETY: the frame data of the running callback.
        unsafe {
            let time = data_frame_time(data);
            assert_eq!(data_timelines_length(data), 1);
            assert_eq!(data_preferred_timeline(data), 0);
            assert_eq!(data_deadline(data, 0), time + FRAME_INTERVAL_NANOS);
            assert_eq!(
                data_expected_presentation(data, 0),
                time + FRAME_INTERVAL_NANOS
            );
            assert!(data_vsync_id(data, 0) > 0);
            assert_eq!(data_deadline(data, 1), 0, "only one timeline");
            seen("vsync", time);
        }
    }

    unsafe extern "C" fn refresh(period: i64, _: *mut c_void) {
        seen("refresh", period);
    }

    /// Polls the thread's looper until `count` callbacks ran or a second passed.
    fn poll_until(count: usize) {
        let end = monotonic_nanos() + 1_000_000_000;
        while mine().len() < count && monotonic_nanos() < end {
            // SAFETY: plain polling with null outputs.
            unsafe {
                looper::poll_once(
                    50,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
        }
    }

    fn on_a_looper_thread(test: impl FnOnce() + Send + 'static) {
        std::thread::spawn(move || {
            // SAFETY: no preconditions.
            assert!(!unsafe { looper::prepare(0) }.is_null());
            test();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_thread_without_a_looper_has_no_choreographer() {
        std::thread::spawn(|| {
            // SAFETY: no preconditions.
            assert!(unsafe { get_instance() }.is_null());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn frame_and_vsync_callbacks_run_on_the_next_vsync_with_its_time() {
        on_a_looper_thread(|| {
            // SAFETY: this thread's choreographer and callbacks of the right signatures.
            unsafe {
                let choreographer = get_instance();
                assert!(!choreographer.is_null());
                assert_eq!(get_instance(), choreographer, "one per thread");
                let before = monotonic_nanos();
                post_frame_callback(choreographer, Some(frame), std::ptr::null_mut());
                post_frame_callback64(choreographer, Some(frame64), std::ptr::null_mut());
                post_vsync_callback(choreographer, Some(vsync), std::ptr::null_mut());
                poll_until(3);
                let seen = mine();
                assert_eq!(
                    seen.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
                    ["frame", "frame64", "vsync"]
                );
                let time = seen[0].1;
                assert!(
                    seen.iter().all(|(_, at)| *at == time),
                    "one vsync: {seen:?}"
                );
                // The next tick of the 60 Hz clock, with no frame clock in a unit test.
                assert!(time > before && time - before <= FRAME_INTERVAL_NANOS);
                assert_eq!(time % FRAME_INTERVAL_NANOS, 0);
            }
        });
    }

    #[test]
    fn a_callback_posted_from_a_callback_runs_on_the_following_vsync() {
        on_a_looper_thread(|| {
            // SAFETY: as above.
            unsafe {
                let choreographer = get_instance();
                post_frame_callback64(choreographer, Some(frame64), choreographer);
                poll_until(2);
                let seen = mine();
                assert_eq!(seen.len(), 2, "{seen:?}");
                assert_eq!(seen[1].0, "next");
                assert!(seen[1].1 >= seen[0].1 + FRAME_INTERVAL_NANOS);
            }
        });
    }

    #[test]
    fn a_delayed_callback_waits_for_its_delay() {
        on_a_looper_thread(|| {
            // SAFETY: as above.
            unsafe {
                let choreographer = get_instance();
                let before = monotonic_nanos();
                post_frame_callback_delayed64(
                    choreographer,
                    Some(frame64),
                    std::ptr::null_mut(),
                    100,
                );
                poll_until(1);
                let seen = mine();
                assert_eq!(seen.len(), 1);
                assert!(seen[0].1 >= before + 100_000_000, "ran early: {seen:?}");
            }
        });
    }

    #[test]
    fn refresh_rate_callbacks_hear_the_period_once_until_unregistered() {
        on_a_looper_thread(|| {
            // SAFETY: as above.
            unsafe {
                let choreographer = get_instance();
                register_refresh_rate_callback(choreographer, Some(refresh), std::ptr::null_mut());
                register_refresh_rate_callback(choreographer, Some(refresh), std::ptr::null_mut());
                poll_until(1);
                assert_eq!(mine(), [("refresh", FRAME_INTERVAL_NANOS)]);
                unregister_refresh_rate_callback(
                    choreographer,
                    Some(refresh),
                    std::ptr::null_mut(),
                );
                // A later vsync tells it nothing more.
                post_frame_callback(choreographer, Some(frame), std::ptr::null_mut());
                poll_until(2);
                assert_eq!(
                    mine().iter().filter(|(kind, _)| *kind == "refresh").count(),
                    1
                );
            }
        });
    }

    #[test]
    fn a_choreographer_of_another_thread_is_refused() {
        let theirs = std::thread::spawn(|| {
            // SAFETY: no preconditions.
            unsafe {
                looper::prepare(0);
                get_instance() as usize
            }
        })
        .join()
        .unwrap();
        on_a_looper_thread(move || {
            // SAFETY: a foreign pointer, which must be refused without being read.
            unsafe {
                post_frame_callback(theirs as *mut c_void, Some(frame), std::ptr::null_mut())
            };
            poll_until(1);
            assert!(mine().is_empty());
        });
    }
}
