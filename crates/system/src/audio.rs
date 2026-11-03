//! `android.media.AudioTrack`, `AudioRecord` and the `AudioSystem` values they read, over
//! `uxdroid_audio`. On Android these natives are in `libandroid_runtime` and talk to `audioserver`
//! through Binder; here each track or record is a stream of this process, with no server between.
//!
//! The framework keeps each native object in a `long` field: `mNativeTrackInJavaObj` and
//! `mNativeAudioRecordHandle` hold an id of [`Handles`], never an address.

use std::ffi::CStr;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jni_sys::{
    JNIEnv, jboolean, jbyteArray, jclass, jfloat, jfloatArray, jint, jintArray, jlong, jlongArray,
    jobject, jshortArray, jstring,
};
use uxdroid_audio::{Backend, Config, Format, Input, Output, State};

use crate::handles::Handles;
use crate::{Method, register};

const SUCCESS: jint = 0;
const ERROR: jint = -1;
const BAD_VALUE: jint = -2;
const INVALID_OPERATION: jint = -3;
/// `AudioTrack.ERROR_NATIVESETUP_*` and `AudioRecord.AUDIORECORD_ERROR_SETUP_*`, which share values.
const SETUP_INVALID_CHANNEL_MASK: jint = -17;
const SETUP_INVALID_FORMAT: jint = -18;
const SETUP_NATIVE_INIT_FAILED: jint = -20;

const MODE_STATIC: jint = 0;
/// What `AudioSystem` reports as the primary output: the rate and period streams default to.
const OUTPUT_RATE: u32 = 48_000;

/// `AudioFormat.ENCODING_*` to a sample format.
fn format(encoding: jint) -> Option<Format> {
    Some(match encoding {
        1 | 2 => Format::I16,
        3 => Format::U8,
        4 => Format::F32,
        21 => Format::I24,
        22 => Format::I32,
        _ => return None,
    })
}

/// Channels of an `AudioFormat` position or index mask: one bit each.
fn channels(position_mask: jint, index_mask: jint) -> Option<u16> {
    let mask = if index_mask != 0 {
        index_mask
    } else {
        position_mask
    };
    let count = (mask as u32).count_ones() as u16;
    (1..=uxdroid_audio::MAX_CHANNELS)
        .contains(&count)
        .then_some(count)
}

/// Bytes a stream needs at least: two periods, as the device moves one while the app fills the next.
fn min_buffer(rate: jint, channel_count: jint, encoding: jint) -> jint {
    let (Some(format), Ok(rate), Ok(channels)) = (
        format(encoding),
        u32::try_from(rate),
        u16::try_from(channel_count),
    ) else {
        return ERROR;
    };
    let config = Config {
        rate,
        channels,
        capacity: 0,
    };
    if config.validate().is_err() {
        return ERROR;
    }
    (2 * config.period() as usize * usize::from(channels) * format.bytes()) as jint
}

struct Track {
    output: Output,
    format: Format,
    /// A static track's whole sound, which each play starts over.
    sound: Option<Mutex<Vec<f32>>>,
    /// The output's position when the current static play began.
    base: AtomicU64,
}

struct Record {
    input: Input,
    format: Format,
}

static TRACKS: Handles<Arc<Track>> = Handles::new();
static RECORDS: Handles<Arc<Record>> = Handles::new();
static SESSIONS: AtomicI32 = AtomicI32::new(1);

/// # Safety
/// `env` must be the calling thread's JNI environment and `object` a live reference.
unsafe fn long_field(env: *mut JNIEnv, object: jobject, name: &CStr) -> Option<jlong> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let field = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), c"J".as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if field.is_null() {
            crate::clear_exception(env);
            return None;
        }
        Some((jni.v1_1.GetLongField)(env, object, field))
    }
}

/// # Safety
/// As [`long_field`].
unsafe fn set_long_field(env: *mut JNIEnv, object: jobject, name: &CStr, value: jlong) {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let field = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), c"J".as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if field.is_null() {
            crate::clear_exception(env);
            return;
        }
        (jni.v1_1.SetLongField)(env, object, field, value);
    }
}

const TRACK_FIELD: &CStr = c"mNativeTrackInJavaObj";
const RECORD_FIELD: &CStr = c"mNativeAudioRecordHandle";

/// # Safety
/// As [`long_field`].
unsafe fn track(env: *mut JNIEnv, this: jobject) -> Option<Arc<Track>> {
    // SAFETY: as above.
    TRACKS.get(unsafe { long_field(env, this, TRACK_FIELD) }?)
}

/// # Safety
/// As [`long_field`].
unsafe fn record(env: *mut JNIEnv, this: jobject) -> Option<Arc<Record>> {
    // SAFETY: as above.
    RECORDS.get(unsafe { long_field(env, this, RECORD_FIELD) }?)
}

/// Reads `array[0]`, the in-out value the framework passes as a one-element array.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, `array` an `int[]` or null.
unsafe fn first(env: *mut JNIEnv, array: jintArray) -> Option<jint> {
    if array.is_null() {
        return None;
    }
    let mut value = 0;
    // SAFETY: as above; one element, bounds-checked by ART.
    unsafe {
        let jni = &**env;
        if (jni.v1_1.GetArrayLength)(env, array) < 1 {
            return None;
        }
        (jni.v1_1.GetIntArrayRegion)(env, array, 0, 1, &mut value);
    }
    Some(value)
}

/// # Safety
/// As [`first`].
unsafe fn set_first(env: *mut JNIEnv, array: jintArray, value: jint) {
    // SAFETY: as above.
    unsafe { ((**env).v1_1.SetIntArrayRegion)(env, array, 0, 1, &value) };
}

/// The rate and session in their in-out arrays: an unspecified rate becomes the output's, and a new
/// session gets an id.
///
/// # Safety
/// As [`first`].
unsafe fn rate_and_session(env: *mut JNIEnv, rates: jintArray, sessions: jintArray) -> Option<u32> {
    // SAFETY: as above.
    unsafe {
        let mut rate = first(env, rates)?;
        if rate == 0 {
            rate = OUTPUT_RATE as jint;
            set_first(env, rates, rate);
        }
        if first(env, sessions) == Some(0) {
            set_first(env, sessions, SESSIONS.fetch_add(1, Ordering::Relaxed));
        }
        u32::try_from(rate).ok()
    }
}

/// `int native_setup(Object weakThis, Object attributes, int[] sampleRate, int channelMask, int
/// channelIndexMask, int audioFormat, int bufferSizeInBytes, int mode, int[] sessionId, Parcel
/// attributionSource, long nativeAudioTrack, boolean offload, int encapsulationMode, Object
/// tunerConfiguration, String opPackageName)`
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn track_setup(
    env: *mut JNIEnv,
    this: jobject,
    _weak: jobject,
    _attributes: jobject,
    rates: jintArray,
    channel_mask: jint,
    index_mask: jint,
    encoding: jint,
    buffer_bytes: jint,
    mode: jint,
    sessions: jintArray,
    _attribution: jobject,
    _native: jlong,
    _offload: jboolean,
    _encapsulation: jint,
    _tuner: jobject,
    _package: jstring,
) -> jint {
    let Some(format) = format(encoding) else {
        return SETUP_INVALID_FORMAT;
    };
    let Some(channels) = channels(channel_mask, index_mask) else {
        return SETUP_INVALID_CHANNEL_MASK;
    };
    // SAFETY: ART passes a valid environment and arrays.
    let Some(rate) = (unsafe { rate_and_session(env, rates, sessions) }) else {
        return SETUP_NATIVE_INIT_FAILED;
    };
    let frame = usize::from(channels) * format.bytes();
    let config = Config {
        rate,
        channels,
        capacity: (buffer_bytes.max(0) as usize / frame) as u32,
    };
    let output = match Output::open(Backend::current(), config) {
        Ok(output) => output,
        Err(error) => {
            uxdroid_bridge::report!("uxdroid: AudioTrack: {error}");
            return SETUP_NATIVE_INIT_FAILED;
        }
    };
    let track = Track {
        output,
        format,
        sound: (mode == MODE_STATIC).then(|| Mutex::new(Vec::new())),
        base: AtomicU64::new(0),
    };
    let id = TRACKS.add(Arc::new(track));
    // SAFETY: as above.
    unsafe { set_long_field(env, this, TRACK_FIELD, id) };
    SUCCESS
}

unsafe extern "system" fn track_release(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    unsafe {
        if let Some(id) = long_field(env, this, TRACK_FIELD) {
            set_long_field(env, this, TRACK_FIELD, 0);
            // The stream's thread stops when the last reference goes.
            drop(TRACKS.remove(id));
        }
    }
}

unsafe extern "system" fn track_start(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    let Some(track) = (unsafe { track(env, this) }) else {
        return;
    };
    if let Some(sound) = &track.sound {
        // A static track plays its whole sound from the start, then stops.
        let sound = sound.lock().unwrap_or_else(|p| p.into_inner());
        track.output.stop();
        track.output.flush();
        track.base.store(track.output.position(), Ordering::Relaxed);
        track.output.write(&sound, false);
        track.output.start();
        track.output.stop();
    } else {
        track.output.start();
    }
}

unsafe extern "system" fn track_stop(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    if let Some(track) = unsafe { track(env, this) } {
        track.output.stop();
    }
}

unsafe extern "system" fn track_pause(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    if let Some(track) = unsafe { track(env, this) } {
        track.output.pause();
    }
}

unsafe extern "system" fn track_flush(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    if let Some(track) = unsafe { track(env, this) } {
        track.output.flush();
    }
}

/// Buffers `samples` and returns how many it took.
fn track_write(track: &Track, samples: &[f32], blocking: bool) -> usize {
    match &track.sound {
        Some(sound) => {
            let mut sound = sound.lock().unwrap_or_else(|p| p.into_inner());
            let capacity = track.output.config().capacity as usize
                * usize::from(track.output.config().channels);
            let take = samples.len().min(capacity.saturating_sub(sound.len()));
            sound.extend_from_slice(&samples[..take]);
            take
        }
        None => track.output.write(samples, blocking),
    }
}

/// Copies `size` elements from `offset` of a primitive array, or `None` when out of bounds.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `read` copy a region of `array`'s type.
unsafe fn region<T: Copy + Default>(
    env: *mut JNIEnv,
    array: jobject,
    offset: jint,
    size: jint,
    read: impl FnOnce(jint, jint, *mut T),
) -> Option<Vec<T>> {
    if array.is_null() || offset < 0 || size < 0 {
        return None;
    }
    // SAFETY: as above.
    let length = unsafe { ((**env).v1_1.GetArrayLength)(env, array) };
    if offset.checked_add(size)? > length {
        return None;
    }
    let mut values = vec![T::default(); size as usize];
    read(offset, size, values.as_mut_ptr());
    Some(values)
}

unsafe extern "system" fn track_write_byte(
    env: *mut JNIEnv,
    this: jobject,
    array: jbyteArray,
    offset: jint,
    size: jint,
    _format: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(track) = track(env, this) else {
            return INVALID_OPERATION;
        };
        let Some(bytes) = region::<u8>(env, array, offset, size, |o, s, out| {
            ((**env).v1_1.GetByteArrayRegion)(env, array, o, s, out.cast())
        }) else {
            return BAD_VALUE;
        };
        let mut samples = vec![0.0; bytes.len() / track.format.bytes()];
        track.format.decode(&bytes, &mut samples);
        (track_write(&track, &samples, blocking) * track.format.bytes()) as jint
    }
}

unsafe extern "system" fn track_write_short(
    env: *mut JNIEnv,
    this: jobject,
    array: jshortArray,
    offset: jint,
    size: jint,
    _format: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(track) = track(env, this) else {
            return INVALID_OPERATION;
        };
        let Some(shorts) = region::<i16>(env, array, offset, size, |o, s, out| {
            ((**env).v1_1.GetShortArrayRegion)(env, array, o, s, out)
        }) else {
            return BAD_VALUE;
        };
        let samples: Vec<f32> = shorts.iter().map(|&s| f32::from(s) / 32_768.0).collect();
        track_write(&track, &samples, blocking) as jint
    }
}

unsafe extern "system" fn track_write_float(
    env: *mut JNIEnv,
    this: jobject,
    array: jfloatArray,
    offset: jint,
    size: jint,
    _format: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(track) = track(env, this) else {
            return INVALID_OPERATION;
        };
        let Some(samples) = region::<f32>(env, array, offset, size, |o, s, out| {
            ((**env).v1_1.GetFloatArrayRegion)(env, array, o, s, out)
        }) else {
            return BAD_VALUE;
        };
        track_write(&track, &samples, blocking) as jint
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment and `buffer` a `ByteBuffer`.
unsafe fn direct_bytes(
    env: *mut JNIEnv,
    buffer: jobject,
    position: jint,
    size: jint,
) -> Option<Vec<u8>> {
    // SAFETY: as above; the range is checked against the buffer's capacity.
    unsafe {
        let jni = &**env;
        let address = (jni.v1_4.GetDirectBufferAddress)(env, buffer).cast::<u8>();
        let capacity = (jni.v1_4.GetDirectBufferCapacity)(env, buffer);
        if address.is_null()
            || position < 0
            || size < 0
            || i64::from(position) + i64::from(size) > capacity
        {
            return None;
        }
        Some(std::slice::from_raw_parts(address.add(position as usize), size as usize).to_vec())
    }
}

unsafe extern "system" fn track_write_native(
    env: *mut JNIEnv,
    this: jobject,
    buffer: jobject,
    position: jint,
    size: jint,
    _format: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and buffer.
    unsafe {
        let Some(track) = track(env, this) else {
            return INVALID_OPERATION;
        };
        let Some(bytes) = direct_bytes(env, buffer, position, size) else {
            return BAD_VALUE;
        };
        let mut samples = vec![0.0; bytes.len() / track.format.bytes()];
        track.format.decode(&bytes, &mut samples);
        (track_write(&track, &samples, blocking) * track.format.bytes()) as jint
    }
}

/// Runs `read` on the track, or answers `fallback` when the object has none.
///
/// # Safety
/// `env` must be the calling thread's JNI environment and `this` an `AudioTrack`.
unsafe fn with_track<T>(
    env: *mut JNIEnv,
    this: jobject,
    fallback: T,
    read: impl FnOnce(&Track) -> T,
) -> T {
    // SAFETY: as above.
    match unsafe { track(env, this) } {
        Some(track) => read(&track),
        None => fallback,
    }
}

unsafe extern "system" fn track_position(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe {
        with_track(env, this, 0, |t| {
            (t.output.position() - t.base.load(Ordering::Relaxed)) as jint
        })
    }
}

unsafe extern "system" fn track_latency(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, 0, |t| t.output.latency().as_millis() as jint) }
}

unsafe extern "system" fn track_underruns(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, 0, |t| t.output.underruns() as jint) }
}

unsafe extern "system" fn track_capacity(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, 0, |t| t.output.config().capacity as jint) }
}

/// The buffer cannot shrink, so a smaller request keeps the whole capacity, which Android allows.
unsafe extern "system" fn track_set_buffer_size(
    env: *mut JNIEnv,
    this: jobject,
    _frames: jint,
) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, BAD_VALUE, |t| t.output.config().capacity as jint) }
}

unsafe extern "system" fn track_set_volume(
    env: *mut JNIEnv,
    this: jobject,
    left: jfloat,
    right: jfloat,
) {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, (), |t| t.output.set_volume(left, right)) }
}

unsafe extern "system" fn track_set_rate(env: *mut JNIEnv, this: jobject, rate: jint) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe {
        with_track(env, this, INVALID_OPERATION, |t| {
            if u32::try_from(rate).is_ok_and(|rate| t.output.set_rate(rate)) {
                SUCCESS
            } else {
                BAD_VALUE
            }
        })
    }
}

unsafe extern "system" fn track_rate(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, 0, |t| t.output.rate() as jint) }
}

unsafe extern "system" fn track_timestamp(
    env: *mut JNIEnv,
    this: jobject,
    out: jlongArray,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(track) = track(env, this) else {
            return INVALID_OPERATION;
        };
        if out.is_null() || ((**env).v1_1.GetArrayLength)(env, out) < 2 {
            return BAD_VALUE;
        }
        let (position, time) = presented(&track.output);
        let values = [position as jlong, time];
        ((**env).v1_1.SetLongArrayRegion)(env, out, 0, 2, values.as_ptr());
    }
    SUCCESS
}

/// The frame the device is presenting now, and now on `CLOCK_MONOTONIC`, as `AudioTimestamp` holds.
fn presented(output: &Output) -> (u64, i64) {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid output.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    (output.presented(), now.tv_sec * 1_000_000_000 + now.tv_nsec)
}

/// Marker and period events are not sent; the values are kept so the getters agree.
unsafe extern "system" fn track_set_marker(env: *mut JNIEnv, this: jobject, _frames: jint) -> jint {
    unimplemented_once(c"android.media.AudioTrack marker and period events");
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, INVALID_OPERATION, |_| SUCCESS) }
}

unsafe extern "system" fn track_loop(
    env: *mut JNIEnv,
    this: jobject,
    _start: jint,
    _end: jint,
    _count: jint,
) -> jint {
    unimplemented_once(c"android.media.AudioTrack.setLoopPoints");
    // SAFETY: ART passes a valid environment and object.
    unsafe { with_track(env, this, INVALID_OPERATION, |_| INVALID_OPERATION) }
}

/// Reports an API that is answered without doing what it promises, once per process.
fn unimplemented_once(name: &'static CStr) {
    static SEEN: Mutex<Vec<&'static CStr>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    if !seen.contains(&name) {
        seen.push(name);
        uxdroid_bridge::report!("uxdroid: unimplemented {}", name.to_string_lossy());
    }
}

unsafe extern "system" fn track_reload(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    unsafe {
        with_track(env, this, INVALID_OPERATION, |t| {
            if t.sound.is_none() {
                return INVALID_OPERATION;
            }
            t.output.stop();
            t.output.flush();
            t.base.store(t.output.position(), Ordering::Relaxed);
            SUCCESS
        })
    }
}

unsafe extern "system" fn output_rate(_env: *mut JNIEnv, _class: jclass, _stream: jint) -> jint {
    OUTPUT_RATE as jint
}

unsafe extern "system" fn track_min_buffer(
    _env: *mut JNIEnv,
    _class: jclass,
    rate: jint,
    channel_count: jint,
    encoding: jint,
) -> jint {
    min_buffer(rate, channel_count, encoding)
}

/// `int native_setup(Object weakThis, Object attributes, int[] sampleRate, int channelMask, int
/// channelIndexMask, int audioFormat, int bufferSizeInBytes, int[] sessionId, Parcel
/// attributionSource, long nativeRecordInJavaObj, int maxSharedAudioHistoryMs, int halInputFlags)`
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn record_setup(
    env: *mut JNIEnv,
    this: jobject,
    _weak: jobject,
    _attributes: jobject,
    rates: jintArray,
    channel_mask: jint,
    index_mask: jint,
    encoding: jint,
    buffer_bytes: jint,
    sessions: jintArray,
    _attribution: jobject,
    _native: jlong,
    _history: jint,
    _flags: jint,
) -> jint {
    // SAFETY: ART passes a valid environment.
    if !unsafe { may_record(env) } {
        uxdroid_bridge::report!(
            "uxdroid: AudioRecord refused: the app does not request android.permission.RECORD_AUDIO"
        );
        return SETUP_NATIVE_INIT_FAILED;
    }
    let Some(format) = format(encoding) else {
        return SETUP_INVALID_FORMAT;
    };
    let Some(channels) = channels(channel_mask, index_mask) else {
        return SETUP_INVALID_CHANNEL_MASK;
    };
    // SAFETY: ART passes valid arrays.
    let Some(rate) = (unsafe { rate_and_session(env, rates, sessions) }) else {
        return SETUP_NATIVE_INIT_FAILED;
    };
    let frame = usize::from(channels) * format.bytes();
    let config = Config {
        rate,
        channels,
        capacity: (buffer_bytes.max(0) as usize / frame) as u32,
    };
    let input = match Input::open(Backend::current(), config) {
        Ok(input) => input,
        Err(error) => {
            uxdroid_bridge::report!("uxdroid: AudioRecord: {error}");
            return SETUP_NATIVE_INIT_FAILED;
        }
    };
    let id = RECORDS.add(Arc::new(Record { input, format }));
    // SAFETY: as above.
    unsafe { set_long_field(env, this, RECORD_FIELD, id) };
    SUCCESS
}

/// Whether the app's manifest requests `RECORD_AUDIO`, asked of `dev.uxdroid.system.Permissions`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn may_record(env: *mut JNIEnv) -> bool {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.FindClass)(env, c"dev/uxdroid/system/Permissions".as_ptr());
        if class.is_null() {
            crate::clear_exception(env);
            return false;
        }
        let method = (jni.v1_1.GetStaticMethodID)(
            env,
            class,
            c"requested".as_ptr(),
            c"(Ljava/lang/String;)Z".as_ptr(),
        );
        if method.is_null() {
            crate::clear_exception(env);
            return false;
        }
        let name = crate::new_string(env, "android.permission.RECORD_AUDIO");
        let granted = (jni.v1_1.CallStaticBooleanMethodA)(
            env,
            class,
            method,
            [jni_sys::jvalue { l: name }].as_ptr(),
        );
        (jni.v1_1.DeleteLocalRef)(env, name);
        (jni.v1_1.DeleteLocalRef)(env, class);
        !crate::clear_exception(env) && granted
    }
}

unsafe extern "system" fn record_release(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    unsafe {
        if let Some(id) = long_field(env, this, RECORD_FIELD) {
            set_long_field(env, this, RECORD_FIELD, 0);
            drop(RECORDS.remove(id));
        }
    }
}

unsafe extern "system" fn record_start(
    env: *mut JNIEnv,
    this: jobject,
    _event: jint,
    _session: jint,
) -> jint {
    // SAFETY: ART passes a valid environment and object.
    match unsafe { record(env, this) } {
        Some(record) if record.input.start() => SUCCESS,
        Some(_) => ERROR,
        None => INVALID_OPERATION,
    }
}

unsafe extern "system" fn record_stop(env: *mut JNIEnv, this: jobject) {
    // SAFETY: ART passes a valid environment and object.
    if let Some(record) = unsafe { record(env, this) } {
        record.input.stop();
    }
}

/// Whether `size` elements from `offset` lie inside `array`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment and `array` an array or null.
unsafe fn in_bounds(env: *mut JNIEnv, array: jobject, offset: jint, size: jint) -> bool {
    !array.is_null()
        && offset >= 0
        && size >= 0
        // SAFETY: as above.
        && offset.checked_add(size).is_some_and(|end| end <= unsafe { ((**env).v1_1.GetArrayLength)(env, array) })
}

/// Reads up to `count` samples, or the error a stopped record answers.
fn record_read(record: &Record, count: usize, blocking: bool) -> Result<Vec<f32>, jint> {
    if record.input.state() != State::Playing && record.input.buffered() == 0 {
        return Err(INVALID_OPERATION);
    }
    let mut samples = vec![0.0; count];
    let read = record.input.read(&mut samples, blocking);
    samples.truncate(read);
    Ok(samples)
}

unsafe extern "system" fn record_read_bytes(
    env: *mut JNIEnv,
    this: jobject,
    array: jbyteArray,
    offset: jint,
    size: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(record) = record(env, this) else {
            return INVALID_OPERATION;
        };
        if !in_bounds(env, array, offset, size) {
            return BAD_VALUE;
        }
        let samples = match record_read(&record, size as usize / record.format.bytes(), blocking) {
            Ok(samples) => samples,
            Err(error) => return error,
        };
        let mut bytes = vec![0u8; samples.len() * record.format.bytes()];
        record.format.encode(&samples, &mut bytes);
        ((**env).v1_1.SetByteArrayRegion)(
            env,
            array,
            offset,
            bytes.len() as jint,
            bytes.as_ptr().cast(),
        );
        bytes.len() as jint
    }
}

unsafe extern "system" fn record_read_shorts(
    env: *mut JNIEnv,
    this: jobject,
    array: jshortArray,
    offset: jint,
    size: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(record) = record(env, this) else {
            return INVALID_OPERATION;
        };
        if !in_bounds(env, array, offset, size) {
            return BAD_VALUE;
        }
        let samples = match record_read(&record, size as usize, blocking) {
            Ok(samples) => samples,
            Err(error) => return error,
        };
        let mut bytes = vec![0u8; samples.len() * 2];
        Format::I16.encode(&samples, &mut bytes);
        let shorts: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        ((**env).v1_1.SetShortArrayRegion)(
            env,
            array,
            offset,
            shorts.len() as jint,
            shorts.as_ptr(),
        );
        shorts.len() as jint
    }
}

unsafe extern "system" fn record_read_floats(
    env: *mut JNIEnv,
    this: jobject,
    array: jfloatArray,
    offset: jint,
    size: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and array.
    unsafe {
        let Some(record) = record(env, this) else {
            return INVALID_OPERATION;
        };
        if !in_bounds(env, array, offset, size) {
            return BAD_VALUE;
        }
        let samples = match record_read(&record, size as usize, blocking) {
            Ok(samples) => samples,
            Err(error) => return error,
        };
        ((**env).v1_1.SetFloatArrayRegion)(
            env,
            array,
            offset,
            samples.len() as jint,
            samples.as_ptr(),
        );
        samples.len() as jint
    }
}

unsafe extern "system" fn record_read_direct(
    env: *mut JNIEnv,
    this: jobject,
    buffer: jobject,
    size: jint,
    blocking: jboolean,
) -> jint {
    // SAFETY: ART passes a valid environment, object and buffer; the range is checked.
    unsafe {
        let Some(record) = record(env, this) else {
            return INVALID_OPERATION;
        };
        let jni = &**env;
        let address = (jni.v1_4.GetDirectBufferAddress)(env, buffer).cast::<u8>();
        let capacity = (jni.v1_4.GetDirectBufferCapacity)(env, buffer);
        if address.is_null() || size < 0 || i64::from(size) > capacity {
            return BAD_VALUE;
        }
        let samples = match record_read(&record, size as usize / record.format.bytes(), blocking) {
            Ok(samples) => samples,
            Err(error) => return error,
        };
        let out = std::slice::from_raw_parts_mut(address, size as usize);
        record.format.encode(&samples, out) as jint * record.format.bytes() as jint
    }
}

unsafe extern "system" fn record_capacity(env: *mut JNIEnv, this: jobject) -> jint {
    // SAFETY: ART passes a valid environment and object.
    match unsafe { record(env, this) } {
        Some(record) => record.input.config().capacity as jint,
        None => 0,
    }
}

unsafe extern "system" fn record_set_marker(
    env: *mut JNIEnv,
    this: jobject,
    _frames: jint,
) -> jint {
    unimplemented_once(c"android.media.AudioRecord marker and period events");
    // SAFETY: ART passes a valid environment and object.
    if unsafe { record(env, this) }.is_some() {
        SUCCESS
    } else {
        INVALID_OPERATION
    }
}

/// The host's server owns routing, and a stream's device never changes under it, so there is no
/// routing event to report and no player id to forward.
unsafe extern "system" fn no_routing(_env: *mut JNIEnv, _this: jobject) {}

unsafe extern "system" fn set_player_id(_env: *mut JNIEnv, _this: jobject, _id: jint) {}

unsafe extern "system" fn system_max_rate(_env: *mut JNIEnv, _class: jclass) -> jint {
    uxdroid_audio::MAX_RATE as jint
}

unsafe extern "system" fn system_min_rate(_env: *mut JNIEnv, _class: jclass) -> jint {
    uxdroid_audio::MIN_RATE as jint
}

unsafe extern "system" fn system_max_channels(_env: *mut JNIEnv, _class: jclass) -> jint {
    jint::from(uxdroid_audio::MAX_CHANNELS)
}

unsafe extern "system" fn system_output_rate(_env: *mut JNIEnv, _class: jclass) -> jint {
    OUTPUT_RATE as jint
}

unsafe extern "system" fn system_output_frames(_env: *mut JNIEnv, _class: jclass) -> jint {
    (OUTPUT_RATE / 100) as jint
}

/// Two periods, as each stream queues on the server.
unsafe extern "system" fn system_output_latency(
    _env: *mut JNIEnv,
    _class: jclass,
    _stream: jint,
) -> jint {
    20
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let track: &[Method] = &[
        (
            c"native_setup",
            c"(Ljava/lang/Object;Ljava/lang/Object;[IIIIII[ILandroid/os/Parcel;JZILjava/lang/Object;Ljava/lang/String;)I",
            f!(track_setup),
        ),
        (c"native_finalize", c"()V", f!(track_release)),
        (c"native_release", c"()V", f!(track_release)),
        (c"native_start", c"()V", f!(track_start)),
        (c"native_stop", c"()V", f!(track_stop)),
        (c"native_pause", c"()V", f!(track_pause)),
        (c"native_flush", c"()V", f!(track_flush)),
        (c"native_write_byte", c"([BIIIZ)I", f!(track_write_byte)),
        (c"native_write_short", c"([SIIIZ)I", f!(track_write_short)),
        (c"native_write_float", c"([FIIIZ)I", f!(track_write_float)),
        (c"native_write_native_bytes", c"(Ljava/nio/ByteBuffer;IIIZ)I", f!(track_write_native)),
        (c"native_reload_static", c"()I", f!(track_reload)),
        (c"native_get_buffer_size_frames", c"()I", f!(track_capacity)),
        (c"native_set_buffer_size_frames", c"(I)I", f!(track_set_buffer_size)),
        (c"native_get_buffer_capacity_frames", c"()I", f!(track_capacity)),
        (c"native_setVolume", c"(FF)V", f!(track_set_volume)),
        (c"native_set_playback_rate", c"(I)I", f!(track_set_rate)),
        (c"native_get_playback_rate", c"()I", f!(track_rate)),
        (c"native_set_marker_pos", c"(I)I", f!(track_set_marker)),
        (c"native_set_pos_update_period", c"(I)I", f!(track_set_marker)),
        (c"native_get_position", c"()I", f!(track_position)),
        (c"native_get_latency", c"()I", f!(track_latency)),
        (c"native_get_underrun_count", c"()I", f!(track_underruns)),
        (c"native_get_timestamp", c"([J)I", f!(track_timestamp)),
        (c"native_set_loop", c"(III)I", f!(track_loop)),
        (c"native_get_output_sample_rate", c"(I)I", f!(output_rate)),
        (c"native_get_min_buff_size", c"(III)I", f!(track_min_buffer)),
        (c"native_setPlayerIId", c"(I)V", f!(set_player_id)),
        (c"native_enableDeviceCallback", c"()V", f!(no_routing)),
        (c"native_disableDeviceCallback", c"()V", f!(no_routing)),
    ];
    let record: &[Method] = &[
        (
            c"native_setup",
            c"(Ljava/lang/Object;Ljava/lang/Object;[IIIII[ILandroid/os/Parcel;JII)I",
            f!(record_setup),
        ),
        (c"native_finalize", c"()V", f!(record_release)),
        (c"native_release", c"()V", f!(record_release)),
        (c"native_start", c"(II)I", f!(record_start)),
        (c"native_stop", c"()V", f!(record_stop)),
        (
            c"native_read_in_byte_array",
            c"([BIIZ)I",
            f!(record_read_bytes),
        ),
        (
            c"native_read_in_short_array",
            c"([SIIZ)I",
            f!(record_read_shorts),
        ),
        (
            c"native_read_in_float_array",
            c"([FIIZ)I",
            f!(record_read_floats),
        ),
        (
            c"native_read_in_direct_buffer",
            c"(Ljava/lang/Object;IZ)I",
            f!(record_read_direct),
        ),
        (
            c"native_get_buffer_size_in_frames",
            c"()I",
            f!(record_capacity),
        ),
        (c"native_set_marker_pos", c"(I)I", f!(record_set_marker)),
        (
            c"native_set_pos_update_period",
            c"(I)I",
            f!(record_set_marker),
        ),
        (c"native_get_min_buff_size", c"(III)I", f!(track_min_buffer)),
        (c"native_enableDeviceCallback", c"()V", f!(no_routing)),
        (c"native_disableDeviceCallback", c"()V", f!(no_routing)),
    ];
    let system: &[Method] = &[
        (c"native_getMaxSampleRate", c"()I", f!(system_max_rate)),
        (c"native_getMinSampleRate", c"()I", f!(system_min_rate)),
        (
            c"native_getMaxChannelCount",
            c"()I",
            f!(system_max_channels),
        ),
        (
            c"getPrimaryOutputSamplingRate",
            c"()I",
            f!(system_output_rate),
        ),
        (
            c"getPrimaryOutputFrameCount",
            c"()I",
            f!(system_output_frames),
        ),
        (c"getOutputLatency", c"(I)I", f!(system_output_latency)),
    ];
    // SAFETY: each table matches the framework's declarations in Android 16.
    unsafe {
        register(env, c"android/media/AudioTrack", track)?;
        register(env, c"android/media/AudioRecord", record)?;
        register(env, c"android/media/AudioSystem", system)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_encodings_and_masks_map_to_streams() {
        assert_eq!(format(2), Some(Format::I16));
        assert_eq!(format(1), Some(Format::I16), "ENCODING_DEFAULT is 16-bit");
        assert_eq!(format(4), Some(Format::F32));
        assert_eq!(format(3), Some(Format::U8));
        assert_eq!(format(21), Some(Format::I24));
        assert_eq!(format(22), Some(Format::I32));
        assert_eq!(format(5), None, "AC-3 is not PCM");
        // CHANNEL_OUT_MONO, CHANNEL_OUT_STEREO, CHANNEL_IN_MONO, 7.1, and an index mask of 3.
        assert_eq!(channels(0x4, 0), Some(1));
        assert_eq!(channels(0xc, 0), Some(2));
        assert_eq!(channels(0x10, 0), Some(1));
        assert_eq!(channels(0x18fc, 0), Some(8));
        assert_eq!(channels(0, 0b111), Some(3));
        assert_eq!(channels(0, 0), None);
        assert_eq!(channels(-1, 0), None, "32 channels");
    }

    #[test]
    fn the_minimum_buffer_is_two_periods() {
        // 48 kHz, stereo, 16-bit: two 480-frame periods of four bytes.
        assert_eq!(min_buffer(48_000, 2, 2), 3840);
        assert_eq!(min_buffer(44_100, 1, 4), 2 * 441 * 4);
        assert_eq!(min_buffer(48_000, 2, 5), ERROR);
        assert_eq!(min_buffer(1_000, 2, 2), ERROR);
        assert_eq!(min_buffer(48_000, 0, 2), ERROR);
        assert_eq!(min_buffer(-1, 2, 2), ERROR);
    }
}
