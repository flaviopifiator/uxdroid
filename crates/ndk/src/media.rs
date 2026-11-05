//! `libmediandk.so`: the NDK media API, with signatures and constants from AOSP's NDK headers.
//!
//! `AMediaFormat` is real: a typed key/value store that keeps its keys in insertion order, as
//! AOSP's `AMessage`. A string or buffer handed out points into the format and stays valid until
//! the format is deleted, cleared or copied over, or its key is set again.
//!
//! No codec is offered yet, so every lookup returns null, as Android does for a type it has no
//! codec for, and apps take their software path. Hardware codecs come with streaming (T13), through
//! Vulkan Video, with VA-API and software fallbacks; see [`no_codec`] for where they plug in. Until
//! then no `AMediaCodec` handle exists, so every call on one, null or not, is refused with
//! `AMEDIA_ERROR_INVALID_OBJECT`. Extractors and data sources are objects whose every source is
//! refused as unsupported; there is no DRM or crypto scheme. Formats, extractors and data sources
//! are kept by address in [`Live`], so a stale or forged pointer is refused, never dereferenced.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex, MutexGuard};

use uxdroid_bridge::table::{Export, Table};

use crate::live::Live;

const AMEDIA_OK: i32 = 0;
const AMEDIA_ERROR_BASE: i32 = -10000;
const AMEDIA_ERROR_UNSUPPORTED: i32 = AMEDIA_ERROR_BASE - 2;
const AMEDIA_ERROR_INVALID_OBJECT: i32 = AMEDIA_ERROR_BASE - 3;
const AMEDIA_ERROR_INVALID_PARAMETER: i32 = AMEDIA_ERROR_BASE - 4;
const AMEDIA_ERROR_INVALID_OPERATION: i32 = AMEDIA_ERROR_BASE - 5;
/// `AMEDIACODEC_ERROR_RECOVERABLE` and `AMEDIACODEC_ERROR_TRANSIENT`, action codes of codec errors.
const ACTION_RECOVERABLE: i32 = 2;
const ACTION_TRANSIENT: i32 = 1;

type Handle = *mut c_void;
const NULL: Handle = std::ptr::null_mut();

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// A C string argument, or `None` when null.
///
/// # Safety
/// A non-null `pointer` is a nul-terminated string that outlives the call.
unsafe fn string<'a>(pointer: *const c_char) -> Option<&'a CStr> {
    // SAFETY: by the caller.
    (!pointer.is_null()).then(|| unsafe { CStr::from_ptr(pointer) })
}

/// Writes `value` through an output argument, unless it is null.
///
/// # Safety
/// A non-null `out` is valid for a write of `T`.
unsafe fn put<T>(out: *mut T, value: T) -> bool {
    if out.is_null() {
        return false;
    }
    // SAFETY: by the caller.
    unsafe { out.write(value) };
    true
}

/// Reports a lookup or source this library refuses, once per process and text.
fn report_once(text: String) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut seen = lock(&SEEN);
    if !seen.contains(&text) {
        uxdroid_bridge::report!("uxdroid: libmediandk: {text}");
        seen.push(text);
    }
}

// AMediaFormat.

#[derive(Clone)]
enum Value {
    Int32(i32),
    Int64(i64),
    Size(usize),
    Float(f32),
    Double(f64),
    Rect([i32; 4]),
    String(CString),
    Buffer(Box<[u8]>),
}

#[derive(Default)]
struct Format {
    entries: Vec<(CString, Value)>,
    /// What `toString` last returned, alive until the next call or the format's end.
    text: CString,
}

impl Format {
    fn set(&mut self, name: &CStr, value: Value) {
        match self
            .entries
            .iter_mut()
            .find(|(key, _)| key.as_c_str() == name)
        {
            Some((_, old)) => *old = value,
            None => self.entries.push((name.to_owned(), value)),
        }
    }

    fn get(&self, name: &CStr) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(key, _)| key.as_c_str() == name)
            .map(|(_, value)| value)
    }

    /// AOSP's text, entry by entry with `printf` formats, and its closing brace without an opening one.
    fn describe(&self) -> CString {
        let mut text = Vec::new();
        for (index, (name, value)) in self.entries.iter().enumerate() {
            if index > 0 {
                text.extend_from_slice(b", ");
            }
            text.extend_from_slice(name.to_bytes());
            text.extend_from_slice(b": ");
            let value = match value {
                Value::Int32(v) => format!("int32({v})"),
                Value::Int64(v) => format!("int64({v})"),
                Value::Size(v) => format!("size_t({v})"),
                Value::Float(v) => format!("float({})", printf_f(f64::from(*v))),
                Value::Double(v) => format!("double({})", printf_f(*v)),
                Value::Rect([l, t, r, b]) => format!("Rect({l}, {t}, {r}, {b})"),
                Value::String(v) => {
                    text.extend_from_slice(b"string(");
                    text.extend_from_slice(v.to_bytes());
                    ")".to_string()
                }
                Value::Buffer(_) => "data".to_string(),
            };
            text.extend_from_slice(value.as_bytes());
        }
        text.push(b'}');
        CString::new(text).unwrap_or_default()
    }
}

/// `printf("%f", value)`.
fn printf_f(value: f64) -> String {
    match value {
        v if v.is_nan() && v.is_sign_negative() => "-nan".to_string(),
        v if v.is_nan() => "nan".to_string(),
        v => format!("{v:.6}"),
    }
}

static FORMATS: Live<Mutex<Format>> = Live::new();

/// Runs `action` on a live format, or answers `refused`.
fn with_format<R>(handle: Handle, refused: R, action: impl FnOnce(&mut Format) -> R) -> R {
    match FORMATS.get(handle) {
        Some(format) => action(&mut lock(&format)),
        None => refused,
    }
}

fn new_format_handle() -> Handle {
    FORMATS.add(Arc::new(Mutex::new(Format::default())))
}

extern "C" fn format_new() -> Handle {
    new_format_handle()
}

extern "C" fn format_delete(format: Handle) -> i32 {
    match FORMATS.remove(format) {
        Some(_) => AMEDIA_OK,
        None => AMEDIA_ERROR_INVALID_OBJECT,
    }
}

extern "C" fn format_clear(format: Handle) {
    with_format(format, (), |format| format.entries.clear());
}

extern "C" fn format_copy(to: Handle, from: Handle) -> i32 {
    if to.is_null() || from.is_null() {
        return AMEDIA_ERROR_INVALID_PARAMETER;
    }
    let Some(entries) = with_format(from, None, |from| Some(from.entries.clone())) else {
        return AMEDIA_ERROR_INVALID_OBJECT;
    };
    with_format(to, AMEDIA_ERROR_INVALID_OBJECT, |to| {
        to.entries = entries;
        AMEDIA_OK
    })
}

extern "C" fn format_to_string(format: Handle) -> *const c_char {
    with_format(format, std::ptr::null(), |format| {
        format.text = format.describe();
        format.text.as_ptr()
    })
}

/// Sets `name` to `value` in a live format; a null name is ignored, as on Android.
///
/// # Safety
/// `name` is null or a nul-terminated string.
unsafe fn set_value(format: Handle, name: *const c_char, value: Option<Value>) {
    // SAFETY: by the caller.
    let (Some(name), Some(value)) = (unsafe { string(name) }, value) else {
        return;
    };
    with_format(format, (), |format| format.set(name, value));
}

/// Reads `name` from a live format through `read`, which picks a value of its own type.
///
/// # Safety
/// `name` is null or a nul-terminated string.
unsafe fn get_value<T>(
    format: Handle,
    name: *const c_char,
    read: impl FnOnce(&Value) -> Option<T>,
) -> Option<T> {
    // SAFETY: by the caller.
    let name = unsafe { string(name) }?;
    with_format(format, None, |format| format.get(name).and_then(read))
}

macro_rules! scalars {
    ($($set:ident $get:ident $kind:ident $type:ty;)*) => {$(
        /// # Safety
        /// `name` is null or a nul-terminated string.
        unsafe extern "C" fn $set(format: Handle, name: *const c_char, value: $type) {
            // SAFETY: by the caller.
            unsafe { set_value(format, name, Some(Value::$kind(value))) }
        }

        /// # Safety
        /// `name` is null or a nul-terminated string, and `out` null or writable.
        unsafe extern "C" fn $get(format: Handle, name: *const c_char, out: *mut $type) -> bool {
            if out.is_null() {
                return false;
            }
            // SAFETY: by the caller.
            let value = unsafe {
                get_value(format, name, |value| match value {
                    Value::$kind(value) => Some(*value),
                    _ => None,
                })
            };
            // SAFETY: by the caller.
            value.is_some_and(|value| unsafe { put(out, value) })
        }
    )*};
}

scalars! {
    format_set_i32 format_get_i32 Int32 i32;
    format_set_i64 format_get_i64 Int64 i64;
    format_set_size format_get_size Size usize;
    format_set_f32 format_get_f32 Float f32;
    format_set_f64 format_get_f64 Double f64;
}

/// # Safety
/// `name` and `value` are null or nul-terminated strings.
unsafe extern "C" fn format_set_string(format: Handle, name: *const c_char, value: *const c_char) {
    // SAFETY: by the caller.
    let value = unsafe { string(value) }.map(|value| Value::String(value.to_owned()));
    // SAFETY: by the caller.
    unsafe { set_value(format, name, value) }
}

/// # Safety
/// `name` is null or a nul-terminated string, and `out` null or writable.
unsafe extern "C" fn format_get_string(
    format: Handle,
    name: *const c_char,
    out: *mut *const c_char,
) -> bool {
    if out.is_null() {
        return false;
    }
    // The string lives in the format, which keeps it until its key is set again or the format ends.
    // SAFETY: by the caller.
    let value = unsafe {
        get_value(format, name, |value| match value {
            Value::String(value) => Some(value.as_ptr()),
            _ => None,
        })
    };
    // SAFETY: by the caller.
    value.is_some_and(|value| unsafe { put(out, value) })
}

/// # Safety
/// `name` is null or a nul-terminated string, and `data` null or readable for `size` bytes.
unsafe extern "C" fn format_set_buffer(
    format: Handle,
    name: *const c_char,
    data: *const c_void,
    size: usize,
) {
    let bytes = match (data.is_null(), size) {
        (_, 0) => Some(Box::default()),
        (true, _) => None,
        // SAFETY: by the caller.
        (false, _) => Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) }.into()),
    };
    // SAFETY: by the caller.
    unsafe { set_value(format, name, bytes.map(Value::Buffer)) }
}

/// # Safety
/// `name` is null or a nul-terminated string, and `data` and `size` null or writable.
unsafe extern "C" fn format_get_buffer(
    format: Handle,
    name: *const c_char,
    data: *mut *mut c_void,
    size: *mut usize,
) -> bool {
    if data.is_null() || size.is_null() {
        return false;
    }
    // SAFETY: by the caller.
    let value = unsafe {
        get_value(format, name, |value| match value {
            Value::Buffer(bytes) => Some((bytes.as_ptr().cast_mut().cast(), bytes.len())),
            _ => None,
        })
    };
    let Some((pointer, length)) = value else {
        return false;
    };
    // SAFETY: by the caller.
    unsafe { put(data, pointer) && put(size, length) }
}

/// # Safety
/// `name` is null or a nul-terminated string.
unsafe extern "C" fn format_set_rect(
    format: Handle,
    name: *const c_char,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    let value = Value::Rect([left, top, right, bottom]);
    // SAFETY: by the caller.
    unsafe { set_value(format, name, Some(value)) }
}

/// # Safety
/// `name` is null or a nul-terminated string, and the outputs null or writable.
unsafe extern "C" fn format_get_rect(
    format: Handle,
    name: *const c_char,
    left: *mut i32,
    top: *mut i32,
    right: *mut i32,
    bottom: *mut i32,
) -> bool {
    if [left, top, right, bottom].iter().any(|out| out.is_null()) {
        return false;
    }
    // SAFETY: by the caller.
    let value = unsafe {
        get_value(format, name, |value| match value {
            Value::Rect(rect) => Some(*rect),
            _ => None,
        })
    };
    let Some([l, t, r, b]) = value else {
        return false;
    };
    // SAFETY: by the caller.
    unsafe { put(left, l) && put(top, t) && put(right, r) && put(bottom, b) }
}

/// `const char*`, the type of each `AMEDIAFORMAT_KEY_*` object.
#[repr(transparent)]
pub struct Key(*const c_char);

// SAFETY: it points at an immutable static string.
unsafe impl Sync for Key {}

// AMediaCodec.

/// Where a codec lookup ends. None is offered yet, so each returns null, as Android does for a type
/// without a codec, and the app takes its software path.
///
/// T13 plugs hardware codecs in here, through Vulkan Video with VA-API and software fallbacks: a
/// lookup that finds one keeps it in a `Live<Codec>` and returns its address, and each codec answer
/// below then checks a handle there before refusing it.
fn no_codec(what: &str, name: *const c_char) -> Handle {
    // SAFETY: a codec lookup's argument is null or a string, by the NDK's contract.
    let name = unsafe { string(name) }.map_or("null".into(), CStr::to_string_lossy);
    report_once(format!("no {what} {name}: no codec is offered yet"));
    NULL
}

/// # Safety
/// `mime` is null or a nul-terminated string.
unsafe extern "C" fn codec_create_decoder(mime: *const c_char) -> Handle {
    no_codec("decoder for", mime)
}

/// # Safety
/// `mime` is null or a nul-terminated string.
unsafe extern "C" fn codec_create_encoder(mime: *const c_char) -> Handle {
    no_codec("encoder for", mime)
}

/// # Safety
/// `name` is null or a nul-terminated string.
unsafe extern "C" fn codec_create_by_name(name: *const c_char) -> Handle {
    no_codec("codec named", name)
}

/// Functions that answer the same whatever their arguments, since the object they act on cannot
/// exist yet. None reads its arguments.
macro_rules! answers {
    ($($name:ident($($argument:ty),*) -> $type:ty = $value:expr;)*) => {$(
        extern "C" fn $name($(_: $argument),*) -> $type {
            $value
        }
    )*};
}

const INVALID: i32 = AMEDIA_ERROR_INVALID_OBJECT;

answers! {
    codec_status(Handle) -> i32 = INVALID;
    codec_configure(Handle, Handle, Handle, Handle, u32) -> i32 = INVALID;
    codec_buffer(Handle, usize, Handle) -> Handle = NULL;
    codec_dequeue_input(Handle, i64) -> isize = INVALID as isize;
    codec_queue_input(Handle, usize, i64, usize, u64, u32) -> i32 = INVALID;
    codec_queue_secure_input(Handle, usize, i64, Handle, u64, u32) -> i32 = INVALID;
    codec_dequeue_output(Handle, Handle, i64) -> isize = INVALID as isize;
    codec_format(Handle) -> Handle = NULL;
    codec_release_output(Handle, usize, bool) -> i32 = INVALID;
    codec_with_handle(Handle, Handle) -> i32 = INVALID;
    codec_release_output_at(Handle, usize, i64) -> i32 = INVALID;
    codec_persistent_surface(Handle) -> i32 = AMEDIA_ERROR_UNSUPPORTED;
    codec_buffer_format(Handle, usize) -> Handle = NULL;
    codec_release_name(Handle, Handle) -> () = ();
    // A callback struct passed by value is in memory on x86_64 and behind a pointer on arm64; the
    // signature names the arm64 pointer, and neither is read.
    codec_set_callback(Handle, Handle, Handle) -> i32 = INVALID;
    crypto_info_new(i32, Handle, Handle, i32, Handle, Handle) -> Handle = NULL;
    crypto_info_set_pattern(Handle, Handle) -> () = ();
    crypto_info_subsamples(Handle) -> usize = 0;
    crypto_info_mode(Handle) -> i32 = INVALID;
    crypto_supported(Handle) -> bool = false;
    crypto_new(Handle, Handle, usize) -> Handle = NULL;
    crypto_delete(Handle) -> () = ();
    drm_supported(Handle, Handle) -> bool = false;
    drm_create(Handle) -> Handle = NULL;
    drm_release(Handle) -> () = ();
    drm_with_handle(Handle, Handle) -> i32 = INVALID;
    drm_with_two(Handle, Handle, Handle) -> i32 = INVALID;
    drm_with_three(Handle, Handle, Handle, Handle) -> i32 = INVALID;
    drm_key_request(Handle, Handle, Handle, usize, Handle, i32, Handle, usize, Handle, Handle) -> i32 =
        INVALID;
    drm_key_request_with_url(
        Handle, Handle, Handle, usize, Handle, i32, Handle, usize, Handle, Handle, Handle, Handle
    ) -> i32 = INVALID;
    drm_key_response(Handle, Handle, Handle, usize, Handle) -> i32 = INVALID;
    drm_bytes(Handle, Handle, usize) -> i32 = INVALID;
    drm_set_bytes(Handle, Handle, Handle, usize) -> i32 = INVALID;
    drm_crypt(Handle, Handle, Handle, Handle, Handle, Handle, Handle, usize) -> i32 = INVALID;
    drm_sign(Handle, Handle, Handle, Handle, Handle, usize, Handle, Handle) -> i32 = INVALID;
    drm_verify(Handle, Handle, Handle, Handle, Handle, usize, Handle, usize) -> i32 = INVALID;
}

/// # Safety
/// `out` is null or writable.
unsafe extern "C" fn codec_get_name(_codec: Handle, _out: *mut *mut c_char) -> i32 {
    INVALID
}

extern "C" fn action_recoverable(code: i32) -> bool {
    code == ACTION_RECOVERABLE
}

extern "C" fn action_transient(code: i32) -> bool {
    code == ACTION_TRANSIENT
}

// AMediaExtractor and AMediaDataSource.

/// An extractor without a source: every source is refused, so it never has a track.
struct Extractor(#[allow(dead_code)] u8);

static EXTRACTORS: Live<Extractor> = Live::new();

/// A data source an app made for an extractor; its callbacks are kept by nobody, since no extractor
/// reads.
struct DataSource(#[allow(dead_code)] u8);

static DATA_SOURCES: Live<DataSource> = Live::new();

fn extractor_answer<T>(extractor: Handle, live: T, refused: T) -> T {
    if EXTRACTORS.get(extractor).is_some() {
        live
    } else {
        refused
    }
}

extern "C" fn extractor_new() -> Handle {
    EXTRACTORS.add(Arc::new(Extractor(0)))
}

extern "C" fn extractor_delete(extractor: Handle) -> i32 {
    match EXTRACTORS.remove(extractor) {
        Some(_) => AMEDIA_OK,
        None => INVALID,
    }
}

fn refuse_source(extractor: Handle) -> i32 {
    if EXTRACTORS.get(extractor).is_none() {
        return INVALID;
    }
    report_once("no media extractor is offered yet".into());
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn extractor_source_fd(extractor: Handle, _fd: i32, _offset: i64, _length: i64) -> i32 {
    refuse_source(extractor)
}

extern "C" fn extractor_source(extractor: Handle, _location: Handle) -> i32 {
    refuse_source(extractor)
}

extern "C" fn extractor_track_count(_extractor: Handle) -> usize {
    0
}

extern "C" fn extractor_track_format(_extractor: Handle, _index: usize) -> Handle {
    NULL
}

extern "C" fn extractor_track(extractor: Handle, _index: usize) -> i32 {
    extractor_answer(extractor, AMEDIA_ERROR_INVALID_PARAMETER, INVALID)
}

extern "C" fn extractor_read(_extractor: Handle, _buffer: Handle, _capacity: usize) -> isize {
    -1
}

extern "C" fn extractor_flags(_extractor: Handle) -> u32 {
    u32::MAX
}

extern "C" fn extractor_track_index(_extractor: Handle) -> i32 {
    -1
}

extern "C" fn extractor_minus_one(_extractor: Handle) -> i64 {
    -1
}

extern "C" fn extractor_advance(_extractor: Handle) -> bool {
    false
}

extern "C" fn extractor_seek(extractor: Handle, _position: i64, _mode: i32) -> i32 {
    extractor_answer(extractor, AMEDIA_ERROR_INVALID_OPERATION, INVALID)
}

extern "C" fn extractor_null(_extractor: Handle) -> Handle {
    NULL
}

/// An empty format, which is what a source without metadata has, as Android returns a new one.
extern "C" fn extractor_file_format(extractor: Handle) -> Handle {
    if EXTRACTORS.get(extractor).is_some() {
        new_format_handle()
    } else {
        NULL
    }
}

extern "C" fn extractor_sample_format(extractor: Handle, format: Handle) -> i32 {
    if EXTRACTORS.get(extractor).is_none() {
        INVALID
    } else if format.is_null() {
        AMEDIA_ERROR_INVALID_PARAMETER
    } else {
        AMEDIA_ERROR_INVALID_OPERATION
    }
}

extern "C" fn data_source_new() -> Handle {
    DATA_SOURCES.add(Arc::new(DataSource(0)))
}

extern "C" fn data_source_new_uri(_uri: Handle, _count: i32, _pairs: Handle) -> Handle {
    report_once("no URI data source is offered yet".into());
    NULL
}

extern "C" fn data_source_delete(source: Handle) {
    DATA_SOURCES.remove(source);
}

extern "C" fn data_source_set(_source: Handle, _value: Handle) {}

extern "C" fn data_source_close(_source: Handle) {}

macro_rules! export {
    ($name:literal, $signature:literal, $function:path) => {
        Export::function($name, $signature, $function as *const ())
    };
}

macro_rules! table {
    (functions { $($function:expr,)* } keys { $($key:ident $value:literal,)* }) => {
        mod keys {
            use super::Key;
            $(pub static $key: Key = Key(concat!($value, "\0").as_ptr().cast());)*
        }

        pub static TABLE: Table = Table {
            library: "libmediandk.so",
            exports: &[
                $($function,)*
                $(Export::data(
                    match CStr::from_bytes_with_nul(
                        concat!("AMEDIAFORMAT_KEY_", stringify!($key), "\0").as_bytes(),
                    ) {
                        Ok(name) => name,
                        Err(_) => panic!("a key name holds a nul"),
                    },
                    "ptr",
                    std::ptr::from_ref(&keys::$key).cast(),
                ),)*
            ],
        };
    };
}

table! {
    functions {
        export!(c"AMediaFormat_new", "ptr()", format_new),
        export!(c"AMediaFormat_delete", "i32(ptr)", format_delete),
        export!(c"AMediaFormat_clear", "void(ptr)", format_clear),
        export!(c"AMediaFormat_copy", "i32(ptr,ptr)", format_copy),
        export!(c"AMediaFormat_toString", "ptr(ptr)", format_to_string),
        export!(c"AMediaFormat_getInt32", "u8(ptr,ptr,ptr)", format_get_i32),
        export!(c"AMediaFormat_getInt64", "u8(ptr,ptr,ptr)", format_get_i64),
        export!(c"AMediaFormat_getFloat", "u8(ptr,ptr,ptr)", format_get_f32),
        export!(c"AMediaFormat_getDouble", "u8(ptr,ptr,ptr)", format_get_f64),
        export!(c"AMediaFormat_getSize", "u8(ptr,ptr,ptr)", format_get_size),
        export!(c"AMediaFormat_getString", "u8(ptr,ptr,ptr)", format_get_string),
        export!(c"AMediaFormat_getBuffer", "u8(ptr,ptr,ptr,ptr)", format_get_buffer),
        export!(c"AMediaFormat_getRect", "u8(ptr,ptr,ptr,ptr,ptr,ptr)", format_get_rect),
        export!(c"AMediaFormat_setInt32", "void(ptr,ptr,i32)", format_set_i32),
        export!(c"AMediaFormat_setInt64", "void(ptr,ptr,i64)", format_set_i64),
        export!(c"AMediaFormat_setFloat", "void(ptr,ptr,f32)", format_set_f32),
        export!(c"AMediaFormat_setDouble", "void(ptr,ptr,f64)", format_set_f64),
        export!(c"AMediaFormat_setSize", "void(ptr,ptr,u64)", format_set_size),
        export!(c"AMediaFormat_setString", "void(ptr,ptr,ptr)", format_set_string),
        export!(c"AMediaFormat_setBuffer", "void(ptr,ptr,ptr,u64)", format_set_buffer),
        export!(c"AMediaFormat_setRect", "void(ptr,ptr,i32,i32,i32,i32)", format_set_rect),
        export!(c"AMediaCodec_createCodecByName", "ptr(ptr)", codec_create_by_name),
        export!(c"AMediaCodec_createDecoderByType", "ptr(ptr)", codec_create_decoder),
        export!(c"AMediaCodec_createEncoderByType", "ptr(ptr)", codec_create_encoder),
        export!(c"AMediaCodec_delete", "i32(ptr)", codec_status),
        export!(c"AMediaCodec_configure", "i32(ptr,ptr,ptr,ptr,u32)", codec_configure),
        export!(c"AMediaCodec_start", "i32(ptr)", codec_status),
        export!(c"AMediaCodec_stop", "i32(ptr)", codec_status),
        export!(c"AMediaCodec_flush", "i32(ptr)", codec_status),
        export!(c"AMediaCodec_getInputBuffer", "ptr(ptr,u64,ptr)", codec_buffer),
        export!(c"AMediaCodec_getOutputBuffer", "ptr(ptr,u64,ptr)", codec_buffer),
        export!(c"AMediaCodec_dequeueInputBuffer", "i64(ptr,i64)", codec_dequeue_input),
        export!(
            c"AMediaCodec_queueInputBuffer",
            "i32(ptr,u64,i64,u64,u64,u32)",
            codec_queue_input
        ),
        export!(
            c"AMediaCodec_queueSecureInputBuffer",
            "i32(ptr,u64,i64,ptr,u64,u32)",
            codec_queue_secure_input
        ),
        export!(c"AMediaCodec_dequeueOutputBuffer", "i64(ptr,ptr,i64)", codec_dequeue_output),
        export!(c"AMediaCodec_getOutputFormat", "ptr(ptr)", codec_format),
        export!(c"AMediaCodec_getInputFormat", "ptr(ptr)", codec_format),
        export!(c"AMediaCodec_getBufferFormat", "ptr(ptr,u64)", codec_buffer_format),
        export!(c"AMediaCodec_releaseOutputBuffer", "i32(ptr,u64,u8)", codec_release_output),
        export!(
            c"AMediaCodec_releaseOutputBufferAtTime",
            "i32(ptr,u64,i64)",
            codec_release_output_at
        ),
        export!(c"AMediaCodec_setOutputSurface", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodec_createInputSurface", "i32(ptr,ptr)", codec_with_handle),
        export!(
            c"AMediaCodec_createPersistentInputSurface",
            "i32(ptr)",
            codec_persistent_surface
        ),
        export!(c"AMediaCodec_setInputSurface", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodec_setParameters", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodec_signalEndOfInputStream", "i32(ptr)", codec_status),
        export!(c"AMediaCodec_getName", "i32(ptr,ptr)", codec_get_name),
        export!(c"AMediaCodec_releaseName", "void(ptr,ptr)", codec_release_name),
        export!(c"AMediaCodec_setAsyncNotifyCallback", "i32(ptr,ptr,ptr)", codec_set_callback),
        export!(
            c"AMediaCodec_setOnFrameRenderedCallback",
            "i32(ptr,ptr,ptr)",
            codec_set_callback
        ),
        export!(c"AMediaCodec_releaseCrypto", "i32(ptr)", codec_status),
        export!(c"AMediaCodecActionCode_isRecoverable", "u8(i32)", action_recoverable),
        export!(c"AMediaCodecActionCode_isTransient", "u8(i32)", action_transient),
        export!(
            c"AMediaCodecCryptoInfo_new",
            "ptr(i32,ptr,ptr,i32,ptr,ptr)",
            crypto_info_new
        ),
        export!(c"AMediaCodecCryptoInfo_delete", "i32(ptr)", codec_status),
        export!(c"AMediaCodecCryptoInfo_setPattern", "void(ptr,ptr)", crypto_info_set_pattern),
        export!(c"AMediaCodecCryptoInfo_getNumSubSamples", "u64(ptr)", crypto_info_subsamples),
        export!(c"AMediaCodecCryptoInfo_getKey", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodecCryptoInfo_getIV", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodecCryptoInfo_getMode", "i32(ptr)", crypto_info_mode),
        export!(c"AMediaCodecCryptoInfo_getClearBytes", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaCodecCryptoInfo_getEncryptedBytes", "i32(ptr,ptr)", codec_with_handle),
        export!(c"AMediaDataSource_new", "ptr()", data_source_new),
        export!(c"AMediaDataSource_newUri", "ptr(ptr,i32,ptr)", data_source_new_uri),
        export!(c"AMediaDataSource_delete", "void(ptr)", data_source_delete),
        export!(c"AMediaDataSource_setUserdata", "void(ptr,ptr)", data_source_set),
        export!(c"AMediaDataSource_setReadAt", "void(ptr,ptr)", data_source_set),
        export!(c"AMediaDataSource_setGetSize", "void(ptr,ptr)", data_source_set),
        export!(c"AMediaDataSource_setClose", "void(ptr,ptr)", data_source_set),
        export!(c"AMediaDataSource_setGetAvailableSize", "void(ptr,ptr)", data_source_set),
        export!(c"AMediaDataSource_close", "void(ptr)", data_source_close),
        export!(c"AMediaExtractor_new", "ptr()", extractor_new),
        export!(c"AMediaExtractor_delete", "i32(ptr)", extractor_delete),
        export!(c"AMediaExtractor_setDataSourceFd", "i32(ptr,i32,i64,i64)", extractor_source_fd),
        export!(c"AMediaExtractor_setDataSource", "i32(ptr,ptr)", extractor_source),
        export!(c"AMediaExtractor_setDataSourceCustom", "i32(ptr,ptr)", extractor_source),
        export!(c"AMediaExtractor_getTrackCount", "u64(ptr)", extractor_track_count),
        export!(c"AMediaExtractor_getTrackFormat", "ptr(ptr,u64)", extractor_track_format),
        export!(c"AMediaExtractor_selectTrack", "i32(ptr,u64)", extractor_track),
        export!(c"AMediaExtractor_unselectTrack", "i32(ptr,u64)", extractor_track),
        export!(c"AMediaExtractor_readSampleData", "i64(ptr,ptr,u64)", extractor_read),
        export!(c"AMediaExtractor_getSampleFlags", "u32(ptr)", extractor_flags),
        export!(c"AMediaExtractor_getSampleTrackIndex", "i32(ptr)", extractor_track_index),
        export!(c"AMediaExtractor_getSampleTime", "i64(ptr)", extractor_minus_one),
        export!(c"AMediaExtractor_getSampleSize", "i64(ptr)", extractor_minus_one),
        export!(c"AMediaExtractor_getCachedDuration", "i64(ptr)", extractor_minus_one),
        export!(c"AMediaExtractor_advance", "u8(ptr)", extractor_advance),
        export!(c"AMediaExtractor_seekTo", "i32(ptr,i64,i32)", extractor_seek),
        export!(c"AMediaExtractor_getPsshInfo", "ptr(ptr)", extractor_null),
        export!(c"AMediaExtractor_getSampleCryptoInfo", "ptr(ptr)", extractor_null),
        export!(c"AMediaExtractor_getFileFormat", "ptr(ptr)", extractor_file_format),
        export!(c"AMediaExtractor_getSampleFormat", "i32(ptr,ptr)", extractor_sample_format),
        export!(c"AMediaCrypto_isCryptoSchemeSupported", "u8(ptr)", crypto_supported),
        export!(
            c"AMediaCrypto_requiresSecureDecoderComponent",
            "u8(ptr)",
            crypto_supported
        ),
        export!(c"AMediaCrypto_new", "ptr(ptr,ptr,u64)", crypto_new),
        export!(c"AMediaCrypto_delete", "void(ptr)", crypto_delete),
        export!(c"AMediaDrm_isCryptoSchemeSupported", "u8(ptr,ptr)", drm_supported),
        export!(c"AMediaDrm_createByUUID", "ptr(ptr)", drm_create),
        export!(c"AMediaDrm_release", "void(ptr)", drm_release),
        export!(c"AMediaDrm_setOnEventListener", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_setOnExpirationUpdateListener", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_setOnKeysChangeListener", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_openSession", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_closeSession", "i32(ptr,ptr)", drm_with_handle),
        export!(
            c"AMediaDrm_getKeyRequest",
            "i32(ptr,ptr,ptr,u64,ptr,i32,ptr,u64,ptr,ptr)",
            drm_key_request
        ),
        export!(
            c"AMediaDrm_getKeyRequestWithDefaultUrlAndType",
            "i32(ptr,ptr,ptr,u64,ptr,i32,ptr,u64,ptr,ptr,ptr,ptr)",
            drm_key_request_with_url
        ),
        export!(c"AMediaDrm_provideKeyResponse", "i32(ptr,ptr,ptr,u64,ptr)", drm_key_response),
        export!(c"AMediaDrm_restoreKeys", "i32(ptr,ptr,ptr)", drm_with_two),
        export!(c"AMediaDrm_removeKeys", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_queryKeyStatus", "i32(ptr,ptr,ptr,ptr)", drm_with_three),
        export!(c"AMediaDrm_getProvisionRequest", "i32(ptr,ptr,ptr,ptr)", drm_with_three),
        export!(c"AMediaDrm_provideProvisionResponse", "i32(ptr,ptr,u64)", drm_bytes),
        export!(c"AMediaDrm_getSecureStops", "i32(ptr,ptr,ptr)", drm_with_two),
        export!(c"AMediaDrm_releaseSecureStops", "i32(ptr,ptr)", drm_with_handle),
        export!(c"AMediaDrm_getPropertyString", "i32(ptr,ptr,ptr)", drm_with_two),
        export!(c"AMediaDrm_getPropertyByteArray", "i32(ptr,ptr,ptr)", drm_with_two),
        export!(c"AMediaDrm_setPropertyString", "i32(ptr,ptr,ptr)", drm_with_two),
        export!(c"AMediaDrm_setPropertyByteArray", "i32(ptr,ptr,ptr,u64)", drm_set_bytes),
        export!(c"AMediaDrm_encrypt", "i32(ptr,ptr,ptr,ptr,ptr,ptr,ptr,u64)", drm_crypt),
        export!(c"AMediaDrm_decrypt", "i32(ptr,ptr,ptr,ptr,ptr,ptr,ptr,u64)", drm_crypt),
        export!(c"AMediaDrm_sign", "i32(ptr,ptr,ptr,ptr,ptr,u64,ptr,ptr)", drm_sign),
        export!(c"AMediaDrm_verify", "i32(ptr,ptr,ptr,ptr,ptr,u64,ptr,u64)", drm_verify),
    }
    // The values of AOSP's NdkMediaFormat.cpp (Apache-2.0). Four keys of the newest NDK headers
    // (CSD_VVC, HDR_ST2094_50_INFO, HORIZONTAL_FLIP, VIDEO_BITRATE_LAYERING) are not there yet.
    keys {
    AAC_DRC_ATTENUATION_FACTOR "aac-drc-cut-level",
    AAC_DRC_BOOST_FACTOR "aac-drc-boost-level",
    AAC_DRC_HEAVY_COMPRESSION "aac-drc-heavy-compression",
    AAC_DRC_TARGET_REFERENCE_LEVEL "aac-target-ref-level",
    AAC_ENCODED_TARGET_LEVEL "aac-encoded-target-level",
    AAC_MAX_OUTPUT_CHANNEL_COUNT "aac-max-output-channel_count",
    AAC_PROFILE "aac-profile",
    AAC_SBR_MODE "aac-sbr-mode",
    ALBUM "album",
    ALBUMART "albumart",
    ALBUMARTIST "albumartist",
    ALLOW_FRAME_DROP "allow-frame-drop",
    ARTIST "artist",
    AUDIO_PRESENTATION_INFO "audio-presentation-info",
    AUDIO_PRESENTATION_PRESENTATION_ID "audio-presentation-presentation-id",
    AUDIO_PRESENTATION_PROGRAM_ID "audio-presentation-program-id",
    AUDIO_SESSION_ID "audio-session-id",
    AUTHOR "author",
    BITRATE_MODE "bitrate-mode",
    BITS_PER_SAMPLE "bits-per-sample",
    BIT_RATE "bitrate",
    BUFFER_BATCH_MAX_OUTPUT_SIZE "buffer-batch-max-output-size",
    BUFFER_BATCH_THRESHOLD_OUTPUT_SIZE "buffer-batch-threshold-output-size",
    CAPTURE_RATE "capture-rate",
    CDTRACKNUMBER "cdtracknum",
    CHANNEL_COUNT "channel-count",
    CHANNEL_MASK "channel-mask",
    COLOR_FORMAT "color-format",
    COLOR_RANGE "color-range",
    COLOR_STANDARD "color-standard",
    COLOR_TRANSFER "color-transfer",
    COMPILATION "compilation",
    COMPLEXITY "complexity",
    COMPOSER "composer",
    CREATE_INPUT_SURFACE_SUSPENDED "create-input-buffers-suspended",
    CRYPTO_DEFAULT_IV_SIZE "crypto-default-iv-size",
    CRYPTO_ENCRYPTED_BYTE_BLOCK "crypto-encrypted-byte-block",
    CRYPTO_ENCRYPTED_SIZES "crypto-encrypted-sizes",
    CRYPTO_IV "crypto-iv",
    CRYPTO_KEY "crypto-key",
    CRYPTO_MODE "crypto-mode",
    CRYPTO_PLAIN_SIZES "crypto-plain-sizes",
    CRYPTO_SKIP_BYTE_BLOCK "crypto-skip-byte-block",
    CSD "csd",
    CSD_0 "csd-0",
    CSD_1 "csd-1",
    CSD_2 "csd-2",
    CSD_AVC "csd-avc",
    CSD_HEVC "csd-hevc",
    D263 "d263",
    DATE "date",
    DISCNUMBER "discnum",
    DISPLAY_CROP "crop",
    DISPLAY_HEIGHT "display-height",
    DISPLAY_WIDTH "display-width",
    DURATION "durationUs",
    ENCODER_DELAY "encoder-delay",
    ENCODER_PADDING "encoder-padding",
    ESDS "esds",
    EXIF_OFFSET "exif-offset",
    EXIF_SIZE "exif-size",
    FLAC_COMPRESSION_LEVEL "flac-compression-level",
    FRAME_COUNT "frame-count",
    FRAME_RATE "frame-rate",
    GENRE "genre",
    GRID_COLUMNS "grid-cols",
    GRID_ROWS "grid-rows",
    HAPTIC_CHANNEL_COUNT "haptic-channel-count",
    HDR10_PLUS_INFO "hdr10-plus-info",
    HDR_STATIC_INFO "hdr-static-info",
    HEIGHT "height",
    ICC_PROFILE "icc-profile",
    IMPORTANCE "importance",
    INTRA_REFRESH_PERIOD "intra-refresh-period",
    IS_ADTS "is-adts",
    IS_AUTOSELECT "is-autoselect",
    IS_DEFAULT "is-default",
    IS_FORCED_SUBTITLE "is-forced-subtitle",
    IS_SYNC_FRAME "is-sync-frame",
    I_FRAME_INTERVAL "i-frame-interval",
    LANGUAGE "language",
    LAST_SAMPLE_INDEX_IN_CHUNK "last-sample-index-in-chunk",
    LATENCY "latency",
    LEVEL "level",
    LOCATION "location",
    LOOP "loop",
    LOW_LATENCY "low-latency",
    LYRICIST "lyricist",
    MANUFACTURER "manufacturer",
    MAX_BIT_RATE "max-bitrate",
    MAX_B_FRAMES "max-bframes",
    MAX_FPS_TO_ENCODER "max-fps-to-encoder",
    MAX_HEIGHT "max-height",
    MAX_INPUT_SIZE "max-input-size",
    MAX_PTS_GAP_TO_ENCODER "max-pts-gap-to-encoder",
    MAX_WIDTH "max-width",
    MIME "mime",
    MPEG2_STREAM_HEADER "mpeg2-stream-header",
    MPEGH_COMPATIBLE_SETS "mpegh-compatible-sets",
    MPEGH_PROFILE_LEVEL_INDICATION "mpegh-profile-level-indication",
    MPEGH_REFERENCE_CHANNEL_LAYOUT "mpegh-reference-channel-layout",
    MPEG_USER_DATA "mpeg-user-data",
    OPERATING_RATE "operating-rate",
    PCM_BIG_ENDIAN "pcm-big-endian",
    PCM_ENCODING "pcm-encoding",
    PICTURE_TYPE "picture-type",
    PRIORITY "priority",
    PROFILE "profile",
    PSSH "pssh",
    PUSH_BLANK_BUFFERS_ON_STOP "push-blank-buffers-on-shutdown",
    REPEAT_PREVIOUS_FRAME_AFTER "repeat-previous-frame-after",
    ROTATION "rotation-degrees",
    SAMPLE_FILE_OFFSET "sample-file-offset",
    SAMPLE_RATE "sample-rate",
    SAMPLE_TIME_BEFORE_APPEND "sample-time-before-append",
    SAR_HEIGHT "sar-height",
    SAR_WIDTH "sar-width",
    SEI "sei",
    SLICE_HEIGHT "slice-height",
    SLOW_MOTION_MARKERS "slow-motion-markers",
    STRIDE "stride",
    TARGET_TIME "target-time",
    TEMPORAL_LAYERING "ts-schema",
    TEMPORAL_LAYER_COUNT "temporal-layer-count",
    TEMPORAL_LAYER_ID "temporal-layer-id",
    TEXT_FORMAT_DATA "text-format-data",
    THUMBNAIL_CSD_AV1C "thumbnail-csd-av1c",
    THUMBNAIL_CSD_HEVC "thumbnail-csd-hevc",
    THUMBNAIL_HEIGHT "thumbnail-height",
    THUMBNAIL_TIME "thumbnail-time",
    THUMBNAIL_WIDTH "thumbnail-width",
    TILE_HEIGHT "tile-height",
    TILE_WIDTH "tile-width",
    TIME_US "timeUs",
    TITLE "title",
    TRACK_ID "track-id",
    TRACK_INDEX "track-index",
    VALID_SAMPLES "valid-samples",
    VIDEO_ENCODING_STATISTICS_LEVEL "video-encoding-statistics-level",
    VIDEO_QP_AVERAGE "video-qp-average",
    WIDTH "width",
    XMP_OFFSET "xmp-offset",
    XMP_SIZE "xmp-size",
    YEAR "year",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};

    type Handle = *mut c_void;

    const OK: i32 = 0;
    const INVALID_OBJECT: i32 = -10003;
    const INVALID_PARAMETER: i32 = -10004;
    const UNSUPPORTED: i32 = -10002;

    /// The function app code binds for `name`, as the C type `T`.
    fn export<T>(name: &str) -> T {
        let export = TABLE
            .find(name.as_bytes())
            .unwrap_or_else(|| panic!("libmediandk.so lacks {name}"));
        assert!(!export.is_data(), "{name} is not a function");
        let address = export.bind().unwrap();
        assert_eq!(size_of::<T>(), size_of::<usize>());
        // SAFETY: `T` is the function pointer type of the export's signature, by each caller.
        unsafe { std::mem::transmute_copy(&address) }
    }

    /// The string an `AMEDIAFORMAT_KEY_*` object points at.
    fn key(name: &str) -> &'static CStr {
        let export = TABLE
            .find(name.as_bytes())
            .unwrap_or_else(|| panic!("libmediandk.so lacks {name}"));
        assert!(export.is_data(), "{name} is not an object");
        let address = export.bind().unwrap() as *const *const c_char;
        // SAFETY: a key object is a pointer to a static string.
        unsafe { CStr::from_ptr(*address) }
    }

    struct Format {
        new: extern "C" fn() -> Handle,
        delete: extern "C" fn(Handle) -> i32,
        copy: extern "C" fn(Handle, Handle) -> i32,
        clear: extern "C" fn(Handle),
        to_string: extern "C" fn(Handle) -> *const c_char,
        set_i32: extern "C" fn(Handle, *const c_char, i32),
        get_i32: extern "C" fn(Handle, *const c_char, *mut i32) -> bool,
        set_i64: extern "C" fn(Handle, *const c_char, i64),
        get_i64: extern "C" fn(Handle, *const c_char, *mut i64) -> bool,
        set_f32: extern "C" fn(Handle, *const c_char, f32),
        get_f32: extern "C" fn(Handle, *const c_char, *mut f32) -> bool,
        set_f64: extern "C" fn(Handle, *const c_char, f64),
        get_f64: extern "C" fn(Handle, *const c_char, *mut f64) -> bool,
        set_size: extern "C" fn(Handle, *const c_char, usize),
        get_size: extern "C" fn(Handle, *const c_char, *mut usize) -> bool,
        set_string: extern "C" fn(Handle, *const c_char, *const c_char),
        get_string: extern "C" fn(Handle, *const c_char, *mut *const c_char) -> bool,
        set_buffer: extern "C" fn(Handle, *const c_char, *const c_void, usize),
        get_buffer: extern "C" fn(Handle, *const c_char, *mut *mut c_void, *mut usize) -> bool,
        set_rect: extern "C" fn(Handle, *const c_char, i32, i32, i32, i32),
        get_rect:
            extern "C" fn(Handle, *const c_char, *mut i32, *mut i32, *mut i32, *mut i32) -> bool,
    }

    fn format() -> Format {
        Format {
            new: export("AMediaFormat_new"),
            delete: export("AMediaFormat_delete"),
            copy: export("AMediaFormat_copy"),
            clear: export("AMediaFormat_clear"),
            to_string: export("AMediaFormat_toString"),
            set_i32: export("AMediaFormat_setInt32"),
            get_i32: export("AMediaFormat_getInt32"),
            set_i64: export("AMediaFormat_setInt64"),
            get_i64: export("AMediaFormat_getInt64"),
            set_f32: export("AMediaFormat_setFloat"),
            get_f32: export("AMediaFormat_getFloat"),
            set_f64: export("AMediaFormat_setDouble"),
            get_f64: export("AMediaFormat_getDouble"),
            set_size: export("AMediaFormat_setSize"),
            get_size: export("AMediaFormat_getSize"),
            set_string: export("AMediaFormat_setString"),
            get_string: export("AMediaFormat_getString"),
            set_buffer: export("AMediaFormat_setBuffer"),
            get_buffer: export("AMediaFormat_getBuffer"),
            set_rect: export("AMediaFormat_setRect"),
            get_rect: export("AMediaFormat_getRect"),
        }
    }

    fn text(pointer: *const c_char) -> String {
        assert!(!pointer.is_null());
        // SAFETY: a string the library handed out, checked non-null.
        unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn every_value_type_round_trips_through_a_format() {
        let f = format();
        let format = (f.new)();
        assert!(!format.is_null());
        (f.set_string)(format, c"mime".as_ptr(), c"video/avc".as_ptr());
        (f.set_i32)(format, c"width".as_ptr(), 1920);
        (f.set_i64)(format, c"durationUs".as_ptr(), -5_000_000_000);
        (f.set_f32)(format, c"frame-rate".as_ptr(), 59.5);
        (f.set_f64)(format, c"capture-rate".as_ptr(), 2.25);
        (f.set_size)(format, c"max-input-size".as_ptr(), 1 << 40);
        let csd = [0u8, 0, 0, 1, 0x67];
        (f.set_buffer)(format, c"csd-0".as_ptr(), csd.as_ptr().cast(), csd.len());
        (f.set_rect)(format, c"crop".as_ptr(), 1, 2, 3, 4);

        let (mut i32v, mut i64v, mut f32v, mut f64v, mut size) = (0, 0, 0.0, 0.0, 0);
        assert!((f.get_i32)(format, c"width".as_ptr(), &mut i32v));
        assert_eq!(i32v, 1920);
        assert!((f.get_i64)(format, c"durationUs".as_ptr(), &mut i64v));
        assert_eq!(i64v, -5_000_000_000);
        assert!((f.get_f32)(format, c"frame-rate".as_ptr(), &mut f32v));
        assert_eq!(f32v, 59.5);
        assert!((f.get_f64)(format, c"capture-rate".as_ptr(), &mut f64v));
        assert_eq!(f64v, 2.25);
        assert!((f.get_size)(format, c"max-input-size".as_ptr(), &mut size));
        assert_eq!(size, 1 << 40);
        let mut string = std::ptr::null();
        assert!((f.get_string)(format, c"mime".as_ptr(), &mut string));
        assert_eq!(text(string), "video/avc");
        let (mut data, mut length) = (std::ptr::null_mut(), 0);
        assert!((f.get_buffer)(
            format,
            c"csd-0".as_ptr(),
            &mut data,
            &mut length
        ));
        // SAFETY: the buffer the format holds, of the length it reported.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) },
            csd
        );
        let mut rect = [0; 4];
        let [l, t, r, b] = &mut rect;
        assert!((f.get_rect)(format, c"crop".as_ptr(), l, t, r, b));
        assert_eq!(rect, [1, 2, 3, 4]);

        // Values are typed, as in AMessage, and keys are exact.
        assert!(!(f.get_i64)(format, c"width".as_ptr(), &mut i64v));
        assert!(!(f.get_i32)(format, c"Width".as_ptr(), &mut i32v));
        assert!(!(f.get_string)(format, c"height".as_ptr(), &mut string));

        assert_eq!(
            text((f.to_string)(format)),
            "mime: string(video/avc), width: int32(1920), durationUs: int64(-5000000000), \
             frame-rate: float(59.500000), capture-rate: double(2.250000), \
             max-input-size: size_t(1099511627776), csd-0: data, crop: Rect(1, 2, 3, 4)}"
        );
        assert_eq!((f.delete)(format), OK);
    }

    #[test]
    fn handed_out_strings_live_until_their_key_is_overwritten() {
        let f = format();
        let format = (f.new)();
        (f.set_string)(format, c"mime".as_ptr(), c"audio/opus".as_ptr());
        let (mut first, mut second) = (std::ptr::null(), std::ptr::null());
        assert!((f.get_string)(format, c"mime".as_ptr(), &mut first));
        // Other keys and other reads leave it alone.
        (f.set_string)(format, c"language".as_ptr(), c"und".as_ptr());
        (f.set_i32)(format, c"sample-rate".as_ptr(), 48_000);
        assert!((f.get_string)(format, c"mime".as_ptr(), &mut second));
        assert_eq!(text(first), "audio/opus");
        assert_eq!(first, second);

        // Overwritten, in place: the order of keys is kept, and a new type replaces the old one.
        (f.set_string)(format, c"mime".as_ptr(), c"audio/raw".as_ptr());
        assert!((f.get_string)(format, c"mime".as_ptr(), &mut second));
        assert_eq!(text(second), "audio/raw");
        (f.set_i32)(format, c"language".as_ptr(), 3);
        assert_eq!(
            text((f.to_string)(format)),
            "mime: string(audio/raw), language: int32(3), sample-rate: int32(48000)}"
        );
        assert_eq!((f.delete)(format), OK);
    }

    #[test]
    fn copy_replaces_the_destination_and_clear_empties_it() {
        let f = format();
        let (from, to) = ((f.new)(), (f.new)());
        (f.set_i32)(from, c"width".as_ptr(), 640);
        (f.set_string)(from, c"mime".as_ptr(), c"video/hevc".as_ptr());
        (f.set_i32)(to, c"stale".as_ptr(), 1);
        assert_eq!((f.copy)(to, from), OK);
        let (mut value, mut string) = (0, std::ptr::null());
        assert!(!(f.get_i32)(to, c"stale".as_ptr(), &mut value));
        assert!((f.get_i32)(to, c"width".as_ptr(), &mut value));
        assert_eq!(value, 640);
        assert!((f.get_string)(to, c"mime".as_ptr(), &mut string));
        assert_eq!(text(string), "video/hevc");
        // The copy is independent of its source.
        (f.set_i32)(from, c"width".as_ptr(), 1);
        assert!((f.get_i32)(to, c"width".as_ptr(), &mut value));
        assert_eq!(value, 640);

        (f.clear)(to);
        assert!(!(f.get_i32)(to, c"width".as_ptr(), &mut value));
        assert_eq!(text((f.to_string)(to)), "}");
        assert_eq!((f.delete)(from), OK);
        assert_eq!((f.delete)(to), OK);
    }

    #[test]
    fn the_format_keys_are_the_platform_strings() {
        assert_eq!(key("AMEDIAFORMAT_KEY_MIME"), c"mime");
        assert_eq!(key("AMEDIAFORMAT_KEY_WIDTH"), c"width");
        assert_eq!(key("AMEDIAFORMAT_KEY_CSD_0"), c"csd-0");
        assert_eq!(key("AMEDIAFORMAT_KEY_COLOR_FORMAT"), c"color-format");
        assert_eq!(
            key("AMEDIAFORMAT_KEY_I_FRAME_INTERVAL"),
            c"i-frame-interval"
        );
        assert_eq!(
            key("AMEDIAFORMAT_KEY_BUFFER_BATCH_MAX_OUTPUT_SIZE"),
            c"buffer-batch-max-output-size"
        );
    }

    #[test]
    fn null_and_forged_formats_are_refused() {
        let f = format();
        let forged = 0x1000 as Handle;
        for handle in [std::ptr::null_mut(), forged] {
            let (mut value, mut string) = (7, std::ptr::null());
            assert!(!(f.get_i32)(handle, c"width".as_ptr(), &mut value));
            assert_eq!(value, 7);
            assert!(!(f.get_string)(handle, c"mime".as_ptr(), &mut string));
            (f.set_i32)(handle, c"width".as_ptr(), 1);
            (f.set_string)(handle, c"mime".as_ptr(), c"x".as_ptr());
            (f.clear)(handle);
            assert!((f.to_string)(handle).is_null());
            assert_eq!((f.delete)(handle), INVALID_OBJECT);
        }
        let format = (f.new)();
        assert_eq!((f.copy)(std::ptr::null_mut(), format), INVALID_PARAMETER);
        assert_eq!((f.copy)(format, std::ptr::null_mut()), INVALID_PARAMETER);
        assert_eq!((f.copy)(format, forged), INVALID_OBJECT);
        // A null name, value or output is refused, not dereferenced.
        let mut value = 0;
        (f.set_i32)(format, std::ptr::null(), 1);
        (f.set_string)(format, c"mime".as_ptr(), std::ptr::null());
        assert!(!(f.get_i32)(format, std::ptr::null(), &mut value));
        (f.set_i32)(format, c"width".as_ptr(), 1);
        assert!(!(f.get_i32)(
            format,
            c"width".as_ptr(),
            std::ptr::null_mut()
        ));
        assert_eq!((f.delete)(format), OK);
        assert_eq!((f.delete)(format), INVALID_OBJECT);
    }

    #[test]
    fn no_codec_is_offered() {
        let decoder: extern "C" fn(*const c_char) -> Handle =
            export("AMediaCodec_createDecoderByType");
        let encoder: extern "C" fn(*const c_char) -> Handle =
            export("AMediaCodec_createEncoderByType");
        let by_name: extern "C" fn(*const c_char) -> Handle =
            export("AMediaCodec_createCodecByName");
        for mime in [
            c"video/avc",
            c"video/hevc",
            c"audio/opus",
            c"video/x-vnd.on2.vp9",
        ] {
            assert!(decoder(mime.as_ptr()).is_null());
            assert!(encoder(mime.as_ptr()).is_null());
        }
        assert!(by_name(c"c2.android.avc.decoder".as_ptr()).is_null());
        assert!(decoder(std::ptr::null()).is_null());
        assert!(by_name(std::ptr::null()).is_null());
    }

    #[test]
    fn calls_on_null_or_forged_codecs_return_errors() {
        let status: [extern "C" fn(Handle) -> i32; 6] = [
            export("AMediaCodec_delete"),
            export("AMediaCodec_start"),
            export("AMediaCodec_stop"),
            export("AMediaCodec_flush"),
            export("AMediaCodec_signalEndOfInputStream"),
            export("AMediaCodec_releaseCrypto"),
        ];
        let configure: extern "C" fn(Handle, Handle, Handle, Handle, u32) -> i32 =
            export("AMediaCodec_configure");
        let dequeue_input: extern "C" fn(Handle, i64) -> isize =
            export("AMediaCodec_dequeueInputBuffer");
        let dequeue_output: extern "C" fn(Handle, *mut [u8; 24], i64) -> isize =
            export("AMediaCodec_dequeueOutputBuffer");
        let input_buffer: extern "C" fn(Handle, usize, *mut usize) -> *mut u8 =
            export("AMediaCodec_getInputBuffer");
        let output_buffer: extern "C" fn(Handle, usize, *mut usize) -> *mut u8 =
            export("AMediaCodec_getOutputBuffer");
        let queue: extern "C" fn(Handle, usize, i64, usize, u64, u32) -> i32 =
            export("AMediaCodec_queueInputBuffer");
        let release: extern "C" fn(Handle, usize, bool) -> i32 =
            export("AMediaCodec_releaseOutputBuffer");
        let release_at: extern "C" fn(Handle, usize, i64) -> i32 =
            export("AMediaCodec_releaseOutputBufferAtTime");
        let output_format: extern "C" fn(Handle) -> Handle = export("AMediaCodec_getOutputFormat");
        let name: extern "C" fn(Handle, *mut *mut c_char) -> i32 = export("AMediaCodec_getName");
        let callback: extern "C" fn(Handle, Handle, Handle) -> i32 =
            export("AMediaCodec_setOnFrameRenderedCallback");
        for codec in [std::ptr::null_mut(), 0x2000 as Handle] {
            for call in status {
                assert_eq!(call(codec), INVALID_OBJECT);
            }
            let null = std::ptr::null_mut();
            assert_eq!(configure(codec, null, null, null, 0), INVALID_OBJECT);
            assert_eq!(dequeue_input(codec, 0), INVALID_OBJECT as isize);
            let mut info = [0u8; 24];
            assert_eq!(dequeue_output(codec, &mut info, 0), INVALID_OBJECT as isize);
            let mut size = 9;
            assert!(input_buffer(codec, 0, &mut size).is_null());
            assert!(output_buffer(codec, 0, &mut size).is_null());
            assert_eq!(queue(codec, 0, 0, 0, 0, 0), INVALID_OBJECT);
            assert_eq!(release(codec, 0, true), INVALID_OBJECT);
            assert_eq!(release_at(codec, 0, 0), INVALID_OBJECT);
            assert!(output_format(codec).is_null());
            let mut out = std::ptr::null_mut();
            assert_eq!(name(codec, &mut out), INVALID_OBJECT);
            assert!(out.is_null());
            assert_eq!(callback(codec, null, null), INVALID_OBJECT);
        }
    }

    #[test]
    fn extractor_crypto_and_drm_answer_unavailable() {
        let new: extern "C" fn() -> Handle = export("AMediaExtractor_new");
        let delete: extern "C" fn(Handle) -> i32 = export("AMediaExtractor_delete");
        let source: extern "C" fn(Handle, *const c_char) -> i32 =
            export("AMediaExtractor_setDataSource");
        let source_fd: extern "C" fn(Handle, i32, i64, i64) -> i32 =
            export("AMediaExtractor_setDataSourceFd");
        let tracks: extern "C" fn(Handle) -> usize = export("AMediaExtractor_getTrackCount");
        let advance: extern "C" fn(Handle) -> bool = export("AMediaExtractor_advance");
        let read: extern "C" fn(Handle, *mut u8, usize) -> isize =
            export("AMediaExtractor_readSampleData");
        let extractor = new();
        assert!(!extractor.is_null());
        assert_eq!(source(extractor, c"/sdcard/a.mp4".as_ptr()), UNSUPPORTED);
        assert_eq!(source_fd(extractor, 0, 0, 10), UNSUPPORTED);
        assert_eq!(tracks(extractor), 0);
        assert!(!advance(extractor));
        let mut buffer = [0u8; 4];
        assert_eq!(read(extractor, buffer.as_mut_ptr(), 4), -1);
        assert_eq!(delete(extractor), OK);
        for handle in [extractor, std::ptr::null_mut()] {
            assert_eq!(source(handle, c"/a".as_ptr()), INVALID_OBJECT);
            assert_eq!(tracks(handle), 0);
            assert_eq!(delete(handle), INVALID_OBJECT);
        }

        let uuid = [0u8; 16];
        let crypto_supported: extern "C" fn(*const u8) -> bool =
            export("AMediaCrypto_isCryptoSchemeSupported");
        let crypto_new: extern "C" fn(*const u8, *const c_void, usize) -> Handle =
            export("AMediaCrypto_new");
        assert!(!crypto_supported(uuid.as_ptr()));
        assert!(crypto_new(uuid.as_ptr(), std::ptr::null(), 0).is_null());

        let drm_supported: extern "C" fn(*const u8, *const c_char) -> bool =
            export("AMediaDrm_isCryptoSchemeSupported");
        let drm_new: extern "C" fn(*const u8) -> Handle = export("AMediaDrm_createByUUID");
        let open: extern "C" fn(Handle, *mut c_void) -> i32 = export("AMediaDrm_openSession");
        let release: extern "C" fn(Handle) = export("AMediaDrm_release");
        assert!(!drm_supported(uuid.as_ptr(), c"video/mp4".as_ptr()));
        assert!(drm_new(uuid.as_ptr()).is_null());
        let mut session = [0u8; 16];
        assert_eq!(
            open(std::ptr::null_mut(), session.as_mut_ptr().cast()),
            INVALID_OBJECT
        );
        release(std::ptr::null_mut());
    }
}
