//! The natives of AOSP's `InputChannel` and `InputEventReceiver`, which the host library lacks.
//!
//! A channel pair is a `SOCK_SEQPACKET` socket pair, as on Android. The window system keeps the
//! server end and writes [`wire`] records into it; `ViewRootImpl` gets the client end through
//! `IWindowSession.addToDisplay` and makes a `WindowInputEventReceiver` on it. The receiver watches
//! its end on its Looper through `MessageQueue.addOnFileDescriptorEventListener`, the way Android's
//! native receiver watches the fd, and hands each event to `dispatchInputEvent` as a framework
//! `KeyEvent` or `MotionEvent`, made by `dev.uxdroid.system.InputEvents`.
//!
//! What InputDispatcher does after delivery is here too: a key the app does not handle and that
//! has a fallback, such as `ESCAPE`, is sent again as its fallback, `BACK`, flagged as one.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use jni_sys::{JNIEnv, jboolean, jclass, jint, jlong, jobject, jstring, jvalue};

use super::keys::{self, FLAG_FALLBACK};
use super::wire::{self, MAX_RECORD};
use super::{Event, Key, Motion};
use crate::handles::Handles;
use crate::java::{self, Field, Pending};
use crate::{Method, java_string, new_string, register, throw};

/// `MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT`.
const EVENT_INPUT: jint = 1;

/// What both ends of a channel share.
pub struct Pair {
    name: String,
    /// Each end, while some channel holds it; an end closes when its last holder lets go.
    ends: [Weak<OwnedFd>; 2],
    /// The `IBinder` that names the pair, made on first use: a global reference or null.
    token: AtomicPtr<c_void>,
    seq: AtomicU32,
}

impl Drop for Pair {
    fn drop(&mut self) {
        let token = self.token.load(Ordering::Acquire) as jobject;
        // SAFETY: the token is this pair's own global reference or null.
        unsafe { java::delete_globals(&[token]) };
    }
}

/// One end of a pair: what an `InputChannel` holds.
#[derive(Clone)]
pub struct Channel {
    pair: Arc<Pair>,
    end: Arc<OwnedFd>,
    side: usize,
}

impl Channel {
    /// A connected pair: the server end, then the client end.
    pub fn pair(name: &str) -> std::io::Result<(Channel, Channel)> {
        let mut fds = [0; 2];
        // SAFETY: `fds` holds two descriptors to write; the result is checked.
        let status = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: socketpair just returned both descriptors, and nothing else owns them.
        let ends = unsafe {
            [
                Arc::new(OwnedFd::from_raw_fd(fds[0])),
                Arc::new(OwnedFd::from_raw_fd(fds[1])),
            ]
        };
        let pair = Arc::new(Pair {
            name: name.to_string(),
            ends: [Arc::downgrade(&ends[0]), Arc::downgrade(&ends[1])],
            token: AtomicPtr::new(std::ptr::null_mut()),
            seq: AtomicU32::new(0),
        });
        let [server, client] = ends;
        Ok((
            Channel {
                pair: pair.clone(),
                end: server,
                side: 0,
            },
            Channel {
                pair,
                end: client,
                side: 1,
            },
        ))
    }

    fn fd(&self) -> RawFd {
        self.end.as_raw_fd()
    }

    /// The other end, while something still holds it.
    fn peer(&self) -> Option<Channel> {
        let side = 1 - self.side;
        Some(Channel {
            pair: self.pair.clone(),
            end: self.pair.ends[side].upgrade()?,
            side,
        })
    }

    pub fn name(&self) -> &str {
        &self.pair.name
    }

    /// Sends an event to the other end. A full or closed channel drops it, as InputDispatcher
    /// drops events for a window that stopped reading; false then.
    pub fn send(&self, event: &Event) -> bool {
        let seq = self
            .pair
            .seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        let record = wire::encode(seq, event);
        // SAFETY: sends `record` on this end's socket without blocking.
        let sent = unsafe {
            libc::send(
                self.fd(),
                record.as_ptr().cast(),
                record.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        sent == record.len() as isize
    }

    /// The next record for this end, if one is waiting.
    pub fn receive(&self) -> Option<(u32, Event)> {
        let mut buffer = [0u8; MAX_RECORD + 1];
        loop {
            // SAFETY: reads at most the buffer's length from this end's socket without blocking.
            let read = unsafe {
                libc::recv(
                    self.fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            let length = usize::try_from(read).ok()?;
            if length == 0 {
                return None;
            }
            // A record that does not decode is skipped, never trusted.
            if let Some(decoded) = wire::decode(&buffer[..length]) {
                return Some(decoded);
            }
        }
    }
}

static CHANNELS: Handles<Channel> = Handles::new();

/// The channel an `InputChannel` object holds, by its `mPtr`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` an InputChannel or null.
pub unsafe fn channel_of(env: *mut JNIEnv, object: jobject) -> Option<Channel> {
    if object.is_null() {
        return None;
    }
    // SAFETY: as above.
    let handle = unsafe { java::long_field(env, object, c"mPtr") }.ok()?;
    CHANNELS.get(handle)
}

unsafe extern "system" fn open_pair(env: *mut JNIEnv, _class: jclass, name: jstring) -> jobject {
    // SAFETY: ART passes a valid environment and a string or null.
    let name = unsafe { java_string(env, name) }.unwrap_or_default();
    let (server, client) = match Channel::pair(&name) {
        Ok(pair) => pair,
        Err(error) => {
            // SAFETY: as above.
            unsafe {
                throw(
                    env,
                    c"java/lang/RuntimeException",
                    &format!("could not open input channel pair {name}: {error}"),
                )
            };
            return std::ptr::null_mut();
        }
    };
    let handles = [CHANNELS.add(server), CHANNELS.add(client)];
    // SAFETY: as above; the array has room for both handles.
    unsafe {
        let array = ((**env).v1_1.NewLongArray)(env, 2);
        if array.is_null() {
            handles.iter().for_each(|h| drop(CHANNELS.remove(*h)));
            return std::ptr::null_mut();
        }
        ((**env).v1_1.SetLongArrayRegion)(env, array, 0, 2, handles.as_ptr());
        array
    }
}

extern "C" fn channel_free(channel: *mut c_void) {
    drop(CHANNELS.remove(channel as i64));
}

unsafe extern "system" fn channel_finalizer(_env: *mut JNIEnv, _class: jclass) -> jlong {
    channel_free as *const () as jlong
}

unsafe extern "system" fn channel_dispose(_env: *mut JNIEnv, _this: jobject, channel: jlong) {
    drop(CHANNELS.remove(channel));
}

unsafe extern "system" fn channel_dup(_env: *mut JNIEnv, _this: jobject, channel: jlong) -> jlong {
    CHANNELS
        .get(channel)
        .map_or(0, |channel| CHANNELS.add(channel))
}

unsafe extern "system" fn channel_name(
    env: *mut JNIEnv,
    _this: jobject,
    channel: jlong,
) -> jstring {
    let name = CHANNELS
        .get(channel)
        .map_or_else(String::new, |c| c.name().to_string());
    // SAFETY: ART passes a valid environment.
    unsafe { new_string(env, &name) }
}

/// Both ends answer the same token, which names the window to the window manager.
unsafe extern "system" fn channel_token(
    env: *mut JNIEnv,
    _this: jobject,
    channel: jlong,
) -> jobject {
    let Some(channel) = CHANNELS.get(channel) else {
        return std::ptr::null_mut();
    };
    let pair = &channel.pair;
    // SAFETY: ART passes a valid environment; the token is a global reference once stored.
    unsafe {
        let mut token = pair.token.load(Ordering::Acquire) as jobject;
        if token.is_null() {
            let Ok(class) = java::find_class(env, c"android/os/Binder") else {
                return std::ptr::null_mut();
            };
            let Ok(made) = java::new_object(env, class, c"()V", &[]) else {
                return std::ptr::null_mut();
            };
            let made = java::global(env, made);
            match pair.token.compare_exchange(
                std::ptr::null_mut(),
                made.cast(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => token = made,
                Err(existing) => {
                    java::delete_globals(&[made]);
                    token = existing as jobject;
                }
            }
        }
        ((**env).v1_2.NewLocalRef)(env, token)
    }
}

// InputEventReceiver.

/// A key sent with its fallback pending: whether the fallback went down.
struct Sent {
    key: Key,
}

struct Receiver {
    channel: Channel,
    /// WeakReference<InputEventReceiver>, the MessageQueue and the FileDescriptor of the channel:
    /// global references.
    receiver: jobject,
    queue: jobject,
    descriptor: jobject,
    /// Keys with a fallback, by sequence number, until the app finishes them.
    awaiting: Mutex<HashMap<u32, Sent>>,
    /// Scan codes whose fallback is down.
    falling_back: Mutex<Vec<i32>>,
    reported: AtomicBool,
}

// SAFETY: the Java references are global, and the rest is thread-safe.
unsafe impl Send for Receiver {}
// SAFETY: as above.
unsafe impl Sync for Receiver {}

impl Drop for Receiver {
    fn drop(&mut self) {
        if let Some(env) = java::attached_env()
            // SAFETY: an attached thread's environment.
            && !unsafe { ((**env).v1_2.ExceptionCheck)(env) }
        {
            let args = [jvalue { l: self.descriptor }];
            // SAFETY: as above, with this receiver's own references.
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
        unsafe { java::delete_globals(&[self.receiver, self.queue, self.descriptor]) };
    }
}

static RECEIVERS: Handles<Arc<Receiver>> = Handles::new();

/// `dev.uxdroid.system.InputListener` and `InputEvents`, found when the natives are registered.
static LISTENER_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static EVENTS_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn create(
    env: *mut JNIEnv,
    receiver: jobject,
    channel: jobject,
    queue: jobject,
) -> Result<jlong, Pending> {
    // SAFETY: as above.
    let Some(channel) = (unsafe { channel_of(env, channel) }) else {
        // SAFETY: as above.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalArgumentException",
                "the input channel is not valid",
            )
        };
        return Err(Pending);
    };
    let listener_class = LISTENER_CLASS.load(Ordering::Acquire) as jclass;
    // SAFETY: as above; the class reference is global and the constructors match.
    unsafe {
        let descriptor_class = java::find_class(env, c"java/io/FileDescriptor")?;
        let descriptor = java::new_object(env, descriptor_class, c"()V", &[])?;
        java::delete_local(env, descriptor_class);
        java::set_field(env, descriptor, c"descriptor", Field::Int(channel.fd()))?;
        let entry = Arc::new(Receiver {
            channel,
            receiver: java::keep(env, receiver),
            queue: java::keep(env, queue),
            descriptor: java::global(env, descriptor),
            awaiting: Mutex::new(HashMap::new()),
            falling_back: Mutex::new(Vec::new()),
            reported: AtomicBool::new(false),
        });
        let (queue, descriptor) = (entry.queue, entry.descriptor);
        let id = RECEIVERS.add(entry);
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

unsafe extern "system" fn receiver_init(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jobject,
    channel: jobject,
    queue: jobject,
) -> jlong {
    // SAFETY: ART passes a valid environment and the receiver's references.
    unsafe { create(env, receiver, channel, queue) }.unwrap_or(0)
}

unsafe extern "system" fn receiver_dispose(_env: *mut JNIEnv, _class: jclass, receiver: jlong) {
    drop(RECEIVERS.remove(receiver));
}

fn millis(nanos: i64) -> jlong {
    nanos.div_euclid(1_000_000)
}

/// Makes the framework's event for one of ours.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn framework_event(env: *mut JNIEnv, event: &Event) -> Result<jobject, Pending> {
    let class = EVENTS_CLASS.load(Ordering::Acquire) as jclass;
    // SAFETY: as above; the class reference is global and the signatures match InputEvents.
    unsafe {
        match event {
            Event::Key(key) => {
                let args = [
                    jvalue {
                        j: millis(key.down_time),
                    },
                    jvalue {
                        j: millis(key.event_time),
                    },
                    jvalue { i: key.action },
                    jvalue { i: key.key_code },
                    jvalue { i: key.repeat },
                    jvalue { i: key.meta_state },
                    jvalue { i: key.device },
                    jvalue { i: key.scan_code },
                    jvalue { i: key.flags },
                    jvalue { i: key.source },
                ];
                java::call_static_object(
                    env,
                    class,
                    c"key",
                    c"(JJIIIIIIII)Landroid/view/KeyEvent;",
                    &args,
                )
            }
            Event::Motion(motion) => motion_event(env, class, motion),
            Event::Focus(_) => Ok(std::ptr::null_mut()),
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `class` InputEvents.
unsafe fn motion_event(
    env: *mut JNIEnv,
    class: jclass,
    motion: &Motion,
) -> Result<jobject, Pending> {
    let count = motion.pointers.len() as jint;
    let ids: Vec<jint> = motion.pointers.iter().map(|p| p.id).collect();
    let tools: Vec<jint> = motion.pointers.iter().map(|p| p.tool).collect();
    let coords: Vec<f32> = motion
        .pointers
        .iter()
        .flat_map(|p| [p.x, p.y, p.pressure])
        .collect();
    // SAFETY: as above; each array is filled to its length before the call.
    unsafe {
        let jni = &**env;
        let id_array = (jni.v1_1.NewIntArray)(env, count);
        let tool_array = (jni.v1_1.NewIntArray)(env, count);
        let coord_array = (jni.v1_1.NewFloatArray)(env, count * 3);
        if id_array.is_null() || tool_array.is_null() || coord_array.is_null() {
            return Err(Pending);
        }
        (jni.v1_1.SetIntArrayRegion)(env, id_array, 0, count, ids.as_ptr());
        (jni.v1_1.SetIntArrayRegion)(env, tool_array, 0, count, tools.as_ptr());
        (jni.v1_1.SetFloatArrayRegion)(env, coord_array, 0, count * 3, coords.as_ptr());
        let args = [
            jvalue {
                j: millis(motion.down_time),
            },
            jvalue {
                j: millis(motion.event_time),
            },
            jvalue { i: motion.action },
            jvalue {
                i: motion.action_button,
            },
            jvalue {
                i: motion.button_state,
            },
            jvalue {
                i: motion.meta_state,
            },
            jvalue { i: motion.source },
            jvalue { i: motion.device },
            jvalue { l: id_array },
            jvalue { l: tool_array },
            jvalue { l: coord_array },
            jvalue { f: motion.vscroll },
            jvalue { f: motion.hscroll },
        ];
        let made = java::call_static_object(
            env,
            class,
            c"motion",
            c"(JJIIIIII[I[I[FFF)Landroid/view/MotionEvent;",
            &args,
        );
        java::delete_local(env, id_array);
        java::delete_local(env, tool_array);
        java::delete_local(env, coord_array);
        made
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn dispatch(env: *mut JNIEnv, receiver: &Receiver) -> Result<jint, Pending> {
    while let Some((seq, event)) = receiver.channel.receive() {
        // SAFETY: as above; the receiver reference is a WeakReference.
        unsafe {
            let target =
                java::call_object(env, receiver.receiver, c"get", c"()Ljava/lang/Object;", &[])?;
            if target.is_null() {
                return Ok(0);
            }
            let delivered = match &event {
                Event::Focus(focused) => java::call_void(
                    env,
                    target,
                    c"onFocusEvent",
                    c"(Z)V",
                    &[jvalue { z: *focused }],
                ),
                Event::Key(key) => {
                    if keys::fallback(key.key_code).is_some() && key.flags & FLAG_FALLBACK == 0 {
                        receiver
                            .awaiting
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .insert(seq, Sent { key: key.clone() });
                    }
                    deliver(env, target, seq, &event)
                }
                Event::Motion(_) => deliver(env, target, seq, &event),
            };
            java::delete_local(env, target);
            delivered?;
        }
    }
    Ok(EVENT_INPUT)
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `target` an InputEventReceiver.
unsafe fn deliver(
    env: *mut JNIEnv,
    target: jobject,
    seq: u32,
    event: &Event,
) -> Result<(), Pending> {
    // SAFETY: as above.
    unsafe {
        let made = framework_event(env, event)?;
        let result = java::call_void(
            env,
            target,
            c"dispatchInputEvent",
            c"(ILandroid/view/InputEvent;)V",
            &[jvalue { i: seq as jint }, jvalue { l: made }],
        );
        java::delete_local(env, made);
        result
    }
}

/// `InputListener.dispatch(long)`: the channel has records, on the receiver's Looper thread.
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

unsafe extern "system" fn receiver_finish(
    _env: *mut JNIEnv,
    _class: jclass,
    receiver: jlong,
    seq: jint,
    handled: jboolean,
) {
    let Some(receiver) = RECEIVERS.get(receiver) else {
        return;
    };
    let sent = receiver
        .awaiting
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&(seq as u32));
    if let Some(Sent { key }) = sent {
        let mut falling_back = receiver
            .falling_back
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let down = falling_back.iter().position(|scan| *scan == key.scan_code);
        let send = match (key.action, down) {
            (keys::ACTION_DOWN, None) if !handled => {
                falling_back.push(key.scan_code);
                true
            }
            (keys::ACTION_UP, Some(index)) => {
                falling_back.remove(index);
                true
            }
            _ => false,
        };
        if send && let Some(fallback) = keys::fallback(key.key_code) {
            let event = Event::Key(Key {
                key_code: fallback,
                flags: key.flags | FLAG_FALLBACK,
                ..key
            });
            // Sent from the other end, it arrives here as the next record, after this one.
            if let Some(peer) = receiver.channel.peer() {
                peer.send(&event);
            }
        }
    }
    if !receiver.reported.swap(true, Ordering::Relaxed) {
        uxdroid_bridge::report!(
            "uxdroid: first input event finished on {}, handled {}",
            receiver.channel.name(),
            handled
        );
    }
}

unsafe extern "system" fn receiver_probably_has_input(
    _env: *mut JNIEnv,
    _class: jclass,
    receiver: jlong,
) -> jboolean {
    let Some(receiver) = RECEIVERS.get(receiver) else {
        return false;
    };
    let mut byte = 0u8;
    // SAFETY: peeks one byte of this receiver's socket without blocking or consuming it.
    let peeked = unsafe {
        libc::recv(
            receiver.channel.fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_DONTWAIT | libc::MSG_PEEK,
        )
    };
    peeked > 0
}

/// Events are never batched, so there is nothing to consume.
unsafe extern "system" fn receiver_consume_batched(
    _env: *mut JNIEnv,
    _class: jclass,
    _receiver: jlong,
    _frame_time: jlong,
) -> jboolean {
    false
}

unsafe extern "system" fn receiver_report_timeline(
    _env: *mut JNIEnv,
    _class: jclass,
    _receiver: jlong,
    _id: jint,
    _gpu: jlong,
    _present: jlong,
) {
}

unsafe extern "system" fn receiver_dump(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jlong,
    prefix: jstring,
) -> jstring {
    // SAFETY: ART passes a valid environment and a string or null.
    let prefix = unsafe { java_string(env, prefix) }.unwrap_or_default();
    let text = RECEIVERS.get(receiver).map_or_else(
        || format!("{prefix}disposed\n"),
        |r| format!("{prefix}channel {}\n", r.channel.name()),
    );
    // SAFETY: as above.
    unsafe { new_string(env, &text) }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let channel: [Method; 6] = [
        (
            c"nativeOpenInputChannelPair",
            c"(Ljava/lang/String;)[J",
            f!(open_pair),
        ),
        (c"nativeGetFinalizer", c"()J", f!(channel_finalizer)),
        (c"nativeDispose", c"(J)V", f!(channel_dispose)),
        (c"nativeDup", c"(J)J", f!(channel_dup)),
        (
            c"nativeGetToken",
            c"(J)Landroid/os/IBinder;",
            f!(channel_token),
        ),
        (c"nativeGetName", c"(J)Ljava/lang/String;", f!(channel_name)),
    ];
    let receiver: [Method; 7] = [
        (
            c"nativeInit",
            c"(Ljava/lang/ref/WeakReference;Landroid/view/InputChannel;Landroid/os/MessageQueue;)J",
            f!(receiver_init),
        ),
        (c"nativeDispose", c"(J)V", f!(receiver_dispose)),
        (c"nativeFinishInputEvent", c"(JIZ)V", f!(receiver_finish)),
        (
            c"nativeProbablyHasInput",
            c"(J)Z",
            f!(receiver_probably_has_input),
        ),
        (
            c"nativeReportTimeline",
            c"(JIJJ)V",
            f!(receiver_report_timeline),
        ),
        (
            c"nativeConsumeBatchedInputEvents",
            c"(JJ)Z",
            f!(receiver_consume_batched),
        ),
        (
            c"nativeDump",
            c"(JLjava/lang/String;)Ljava/lang/String;",
            f!(receiver_dump),
        ),
    ];
    let listener: [Method; 1] = [(c"dispatch", c"(J)I", f!(listener_dispatch))];
    // SAFETY: each table matches its Java declarations in Android 16.
    unsafe {
        register(env, c"android/view/InputChannel", &channel)?;
        register(env, c"android/view/InputEventReceiver", &receiver)?;
        register(env, c"dev/uxdroid/system/InputListener", &listener)?;
        for (name, slot) in [
            (c"dev/uxdroid/system/InputListener", &LISTENER_CLASS),
            (c"dev/uxdroid/system/InputEvents", &EVENTS_CLASS),
        ] {
            let class = java::find_class(env, name)
                .map_err(|Pending| format!("class {} not found", name.to_string_lossy()))?;
            slot.store(java::global(env, class).cast(), Ordering::Release);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Pointer, SOURCE_MOUSE};

    #[test]
    fn a_pair_carries_events_both_ways_in_order() {
        let (server, client) = Channel::pair("window").expect("socketpair");
        assert_eq!(client.name(), "window");
        assert_eq!(client.receive(), None, "nothing sent yet");
        let click = Event::Motion(Motion {
            down_time: 1,
            event_time: 2,
            action: 0,
            action_button: 0,
            button_state: 1,
            meta_state: 0,
            source: SOURCE_MOUSE,
            device: 2,
            vscroll: 0.0,
            hscroll: 0.0,
            pointers: vec![Pointer {
                id: 0,
                tool: 3,
                x: 5.0,
                y: 6.0,
                pressure: 1.0,
            }],
        });
        assert!(server.send(&Event::Focus(true)));
        assert!(server.send(&click));
        let (first_seq, first) = client.receive().expect("focus");
        let (second_seq, second) = client.receive().expect("click");
        assert_eq!((first, second), (Event::Focus(true), click));
        assert!(second_seq > first_seq && first_seq > 0);
        assert_eq!(client.receive(), None);
        // The reverse direction is how a fallback key reaches its own receiver.
        assert!(
            client
                .peer()
                .expect("server end")
                .send(&Event::Focus(false))
        );
        assert_eq!(client.receive().map(|(_, e)| e), Some(Event::Focus(false)));
    }

    #[test]
    fn garbage_on_the_socket_is_skipped() {
        let (server, client) = Channel::pair("window").expect("socketpair");
        // SAFETY: sends three bytes on the server end.
        unsafe { libc::send(server.fd(), b"bad".as_ptr().cast(), 3, 0) };
        assert!(server.send(&Event::Focus(true)));
        assert_eq!(client.receive().map(|(_, e)| e), Some(Event::Focus(true)));
    }

    #[test]
    fn a_closed_peer_drops_events_without_a_signal() {
        let (server, client) = Channel::pair("window").expect("socketpair");
        drop(client);
        assert!(!server.send(&Event::Focus(true)));
    }

    #[test]
    fn a_full_channel_drops_events_instead_of_blocking() {
        let (server, _client) = Channel::pair("window").expect("socketpair");
        let sent = (0..100_000)
            .take_while(|_| server.send(&Event::Focus(true)))
            .count();
        assert!(sent < 100_000, "the socket buffer is bounded");
        assert!(!server.send(&Event::Focus(true)));
    }
}
