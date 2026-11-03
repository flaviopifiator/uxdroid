//! Natives that Android's system provides and AOSP's host library does not.

use std::sync::OnceLock;

use jni_sys::{JNIEnv, jboolean, jclass, jint, jlong, jobject, jstring};

use crate::properties::{self, Properties};
use crate::{Method, java_string, new_string, register, throw};

pub static PROPERTIES: Properties = Properties::new();

/// Android's log priorities, from VERBOSE (2) to ASSERT (7).
const LEVELS: [&str; 8] = ["?", "?", "V", "D", "I", "W", "E", "F"];

/// The lowest priority printed, from `UXDROID_LOG` as a letter or number. INFO by default, like Android.
pub const DEBUG: jint = 3;

pub fn min_priority() -> jint {
    static MIN: OnceLock<jint> = OnceLock::new();
    *MIN.get_or_init(|| match std::env::var("UXDROID_LOG").ok().as_deref() {
        Some(level) => LEVELS
            .iter()
            .position(|l| l.eq_ignore_ascii_case(level))
            .map(|p| p as jint)
            .or_else(|| level.parse().ok())
            .unwrap_or(4),
        None => 4,
    })
}

unsafe extern "system" fn log_println(
    env: *mut JNIEnv,
    _class: jclass,
    _buffer: jint,
    priority: jint,
    tag: jstring,
    message: jstring,
) -> jint {
    if priority < min_priority() {
        return 0;
    }
    // SAFETY: ART passes a valid environment and strings or null.
    let (tag, message) = unsafe { (java_string(env, tag), java_string(env, message)) };
    let level = LEVELS.get(priority as usize).unwrap_or(&"?");
    let line = format!(
        "{level} {}: {}\n",
        tag.unwrap_or_default(),
        message.unwrap_or_default()
    );
    eprint!("{line}");
    line.len() as jint
}

unsafe extern "system" fn log_is_loggable(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jstring,
    level: jint,
) -> jboolean {
    level >= min_priority()
}

unsafe extern "system" fn log_max_payload(_env: *mut JNIEnv, _class: jclass) -> jint {
    4068
}

// Tracing is off on the desktop.
unsafe extern "system" fn trace_off(_env: *mut JNIEnv, _class: jclass, _tag: jlong) -> jboolean {
    false
}
unsafe extern "system" fn trace_flag(_env: *mut JNIEnv, _class: jclass, _on: jboolean) {}
unsafe extern "system" fn trace_tag(_env: *mut JNIEnv, _class: jclass, _tag: jlong) {}
unsafe extern "system" fn trace_name(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jlong,
    _name: jstring,
) {
}
unsafe extern "system" fn trace_counter(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jlong,
    _name: jstring,
    _value: jlong,
) {
}
unsafe extern "system" fn trace_async(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jlong,
    _name: jstring,
    _cookie: jint,
) {
}
unsafe extern "system" fn trace_track(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jlong,
    _track: jstring,
    _name: jstring,
) {
}
unsafe extern "system" fn trace_track_async(
    _env: *mut JNIEnv,
    _class: jclass,
    _tag: jlong,
    _track: jstring,
    _name: jstring,
    _cookie: jint,
) {
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `key` a valid string or null.
unsafe fn property(env: *mut JNIEnv, key: jstring) -> Option<String> {
    // SAFETY: as above.
    unsafe { java_string(env, key) }.and_then(|key| PROPERTIES.get(&key))
}

unsafe extern "system" fn sp_get(
    env: *mut JNIEnv,
    _class: jclass,
    key: jstring,
    default: jstring,
) -> jstring {
    // SAFETY: ART passes a valid environment and strings.
    match unsafe { property(env, key) } {
        Some(value) if !value.is_empty() => unsafe { new_string(env, &value) },
        _ => default,
    }
}
unsafe extern "system" fn sp_get_int(
    env: *mut JNIEnv,
    _class: jclass,
    key: jstring,
    default: jint,
) -> jint {
    // SAFETY: as above.
    let value = unsafe { property(env, key) };
    properties::parse_int(value.as_deref())
        .and_then(|v| jint::try_from(v).ok())
        .unwrap_or(default)
}
unsafe extern "system" fn sp_get_long(
    env: *mut JNIEnv,
    _class: jclass,
    key: jstring,
    default: jlong,
) -> jlong {
    // SAFETY: as above.
    let value = unsafe { property(env, key) };
    properties::parse_int(value.as_deref()).unwrap_or(default)
}
unsafe extern "system" fn sp_get_bool(
    env: *mut JNIEnv,
    _class: jclass,
    key: jstring,
    default: jboolean,
) -> jboolean {
    // SAFETY: as above.
    let value = unsafe { property(env, key) };
    properties::parse_bool(value.as_deref()).unwrap_or(default)
}
unsafe extern "system" fn sp_find(env: *mut JNIEnv, _class: jclass, key: jstring) -> jlong {
    // SAFETY: as above.
    unsafe { java_string(env, key) }.map_or(0, |key| PROPERTIES.find(&key))
}
unsafe extern "system" fn sp_get_handle(
    env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jstring {
    let value = PROPERTIES.get_handle(handle).unwrap_or_default();
    // SAFETY: as above.
    unsafe { new_string(env, &value) }
}
unsafe extern "system" fn sp_get_int_handle(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
    default: jint,
) -> jint {
    properties::parse_int(PROPERTIES.get_handle(handle).as_deref())
        .and_then(|v| jint::try_from(v).ok())
        .unwrap_or(default)
}
unsafe extern "system" fn sp_get_long_handle(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
    default: jlong,
) -> jlong {
    properties::parse_int(PROPERTIES.get_handle(handle).as_deref()).unwrap_or(default)
}
unsafe extern "system" fn sp_get_bool_handle(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
    default: jboolean,
) -> jboolean {
    properties::parse_bool(PROPERTIES.get_handle(handle).as_deref()).unwrap_or(default)
}
unsafe extern "system" fn sp_set(env: *mut JNIEnv, _class: jclass, key: jstring, value: jstring) {
    // SAFETY: ART passes a valid environment and strings.
    let (key, value) = unsafe { (java_string(env, key), java_string(env, value)) };
    let key = key.unwrap_or_default();
    let value = value.unwrap_or_default();
    if let Err(error) = PROPERTIES.set(&key, &value) {
        let message = format!("failed to set system property \"{key}\" to \"{value}\": {error}");
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/RuntimeException", &message) };
    }
}
unsafe extern "system" fn sp_noop(_env: *mut JNIEnv, _class: jclass) {}

fn clock(id: libc::clockid_t) -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock and output.
    unsafe { libc::clock_gettime(id, &mut time) };
    time.tv_sec * 1_000_000_000 + time.tv_nsec
}
unsafe extern "system" fn uptime_ms(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_MONOTONIC) / 1_000_000
}
unsafe extern "system" fn uptime_ns(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_MONOTONIC)
}
unsafe extern "system" fn elapsed_ms(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_BOOTTIME) / 1_000_000
}
unsafe extern "system" fn elapsed_ns(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_BOOTTIME)
}
unsafe extern "system" fn thread_ms(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_THREAD_CPUTIME_ID) / 1_000_000
}
unsafe extern "system" fn thread_us(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_THREAD_CPUTIME_ID) / 1_000
}
unsafe extern "system" fn now_us(_env: *mut JNIEnv, _class: jclass) -> jlong {
    clock(libc::CLOCK_REALTIME) / 1_000
}

/// A local Binder only needs a handle the allocation registry can free; nothing crosses processes.
unsafe extern "C" fn binder_holder_free(holder: *mut u64) {
    // SAFETY: the registry calls this once, with a pointer from `binder_holder`.
    drop(unsafe { Box::from_raw(holder) });
}
unsafe extern "system" fn binder_holder(_env: *mut JNIEnv, _class: jclass) -> jlong {
    Box::into_raw(Box::new(0u64)) as jlong
}
unsafe extern "system" fn binder_finalizer(_env: *mut JNIEnv, _class: jclass) -> jlong {
    binder_holder_free as unsafe extern "C" fn(*mut u64) as usize as jlong
}
unsafe extern "system" fn calling_pid(_env: *mut JNIEnv, _class: jclass) -> jint {
    std::process::id() as jint
}
unsafe extern "system" fn calling_uid(_env: *mut JNIEnv, _class: jclass) -> jint {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() as jint }
}
unsafe extern "system" fn clear_identity(env: *mut JNIEnv, class: jclass) -> jlong {
    // SAFETY: as above.
    unsafe { ((calling_uid(env, class) as jlong) << 32) | calling_pid(env, class) as u32 as jlong }
}
unsafe extern "system" fn restore_identity(_env: *mut JNIEnv, _class: jclass, _token: jlong) {}
/// No transaction ever arrives from another process, so no thread is handling one.
unsafe extern "system" fn handling_transaction(_env: *mut JNIEnv, _class: jclass) -> jboolean {
    false
}

/// Services live in this process, written in Java: the service manager is how apps find them.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn service_manager(env: *mut JNIEnv) -> jobject {
    // SAFETY: as above. `dev.uxdroid.system.Services` is on the framework's class path.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.FindClass)(env, c"dev/uxdroid/system/Services".as_ptr());
        if class.is_null() {
            return std::ptr::null_mut();
        }
        let method = (jni.v1_1.GetStaticMethodID)(
            env,
            class,
            c"contextObject".as_ptr(),
            c"()Landroid/os/IBinder;".as_ptr(),
        );
        if method.is_null() {
            return std::ptr::null_mut();
        }
        (jni.v1_1.CallStaticObjectMethodA)(env, class, method, std::ptr::null())
    }
}
unsafe extern "system" fn context_object(env: *mut JNIEnv, _class: jclass) -> jobject {
    // SAFETY: as above.
    unsafe { service_manager(env) }
}
unsafe extern "system" fn native_service_manager(env: *mut JNIEnv, _this: jobject) -> jobject {
    // SAFETY: as above.
    unsafe { service_manager(env) }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_all(env: *mut JNIEnv) -> Result<(), String> {
    let log: [Method; 3] = [
        (
            c"println_native",
            c"(IILjava/lang/String;Ljava/lang/String;)I",
            f!(log_println),
        ),
        (
            c"isLoggable",
            c"(Ljava/lang/String;I)Z",
            f!(log_is_loggable),
        ),
        (
            c"logger_entry_max_payload_native",
            c"()I",
            f!(log_max_payload),
        ),
    ];
    let trace: [Method; 12] = [
        (c"nativeIsTagEnabled", c"(J)Z", f!(trace_off)),
        (c"nativeSetAppTracingAllowed", c"(Z)V", f!(trace_flag)),
        (c"nativeSetTracingEnabled", c"(Z)V", f!(trace_flag)),
        (
            c"nativeTraceCounter",
            c"(JLjava/lang/String;J)V",
            f!(trace_counter),
        ),
        (
            c"nativeTraceBegin",
            c"(JLjava/lang/String;)V",
            f!(trace_name),
        ),
        (c"nativeTraceEnd", c"(J)V", f!(trace_tag)),
        (
            c"nativeAsyncTraceBegin",
            c"(JLjava/lang/String;I)V",
            f!(trace_async),
        ),
        (
            c"nativeAsyncTraceEnd",
            c"(JLjava/lang/String;I)V",
            f!(trace_async),
        ),
        (
            c"nativeAsyncTraceForTrackBegin",
            c"(JLjava/lang/String;Ljava/lang/String;I)V",
            f!(trace_track_async),
        ),
        (
            c"nativeAsyncTraceForTrackEnd",
            c"(JLjava/lang/String;I)V",
            f!(trace_async),
        ),
        (c"nativeInstant", c"(JLjava/lang/String;)V", f!(trace_name)),
        (
            c"nativeInstantForTrack",
            c"(JLjava/lang/String;Ljava/lang/String;)V",
            f!(trace_track),
        ),
    ];
    let props: [Method; 12] = [
        (
            c"native_get",
            c"(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            f!(sp_get),
        ),
        (c"native_get_int", c"(Ljava/lang/String;I)I", f!(sp_get_int)),
        (
            c"native_get_long",
            c"(Ljava/lang/String;J)J",
            f!(sp_get_long),
        ),
        (
            c"native_get_boolean",
            c"(Ljava/lang/String;Z)Z",
            f!(sp_get_bool),
        ),
        (c"native_find", c"(Ljava/lang/String;)J", f!(sp_find)),
        (c"native_get", c"(J)Ljava/lang/String;", f!(sp_get_handle)),
        (c"native_get_int", c"(JI)I", f!(sp_get_int_handle)),
        (c"native_get_long", c"(JJ)J", f!(sp_get_long_handle)),
        (c"native_get_boolean", c"(JZ)Z", f!(sp_get_bool_handle)),
        (
            c"native_set",
            c"(Ljava/lang/String;Ljava/lang/String;)V",
            f!(sp_set),
        ),
        (c"native_add_change_callback", c"()V", f!(sp_noop)),
        (c"native_report_sysprop_change", c"()V", f!(sp_noop)),
    ];
    let clock: [Method; 7] = [
        (c"uptimeMillis", c"()J", f!(uptime_ms)),
        (c"uptimeNanos", c"()J", f!(uptime_ns)),
        (c"elapsedRealtime", c"()J", f!(elapsed_ms)),
        (c"elapsedRealtimeNanos", c"()J", f!(elapsed_ns)),
        (c"currentThreadTimeMillis", c"()J", f!(thread_ms)),
        (c"currentThreadTimeMicro", c"()J", f!(thread_us)),
        (c"currentTimeMicro", c"()J", f!(now_us)),
    ];
    let binder: [Method; 7] = [
        (c"getNativeBBinderHolder", c"()J", f!(binder_holder)),
        (c"getNativeFinalizer", c"()J", f!(binder_finalizer)),
        (c"getCallingPid", c"()I", f!(calling_pid)),
        (c"getCallingUid", c"()I", f!(calling_uid)),
        (c"clearCallingIdentity", c"()J", f!(clear_identity)),
        (c"restoreCallingIdentity", c"(J)V", f!(restore_identity)),
        (
            c"isDirectlyHandlingTransactionNative",
            c"()Z",
            f!(handling_transaction),
        ),
    ];
    let binder_internal: [Method; 1] = [(
        c"getContextObject",
        c"()Landroid/os/IBinder;",
        f!(context_object),
    )];
    let service_manager_proxy: [Method; 1] = [(
        c"getNativeServiceManager",
        c"()Landroid/os/IBinder;",
        f!(native_service_manager),
    )];
    // SAFETY: each table matches the framework's declarations in Android 16.
    unsafe {
        register(env, c"android/util/Log", &log)?;
        register(env, c"android/os/Trace", &trace)?;
        register(env, c"android/os/SystemProperties", &props)?;
        register(env, c"android/os/SystemClock", &clock)?;
        register(env, c"android/os/Binder", &binder)?;
        register(
            env,
            c"com/android/internal/os/BinderInternal",
            &binder_internal,
        )?;
        register(
            env,
            c"android/os/ServiceManagerProxy",
            &service_manager_proxy,
        )?;
        crate::parcel::register_natives(env)?;
        crate::shared_memory::register_natives(env)?;
        crate::process::register_natives(env)?;
        crate::surface::register_natives(env)?;
        crate::vsync::register_natives(env)?;
    }
    Ok(())
}
