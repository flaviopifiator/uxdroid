//! The natives of `android.os.Parcel`, after AOSP's `core/jni/android_os_Parcel.cpp`.
//!
//! `mNativePtr` is the address of a boxed [`Parcel`], as on Android, where it is the address of a
//! C++ `Parcel`: the class calls these natives on its hottest paths, every `writeInt`, so they do
//! no lookup. A binder is a global reference to its Java object, shared by every parcel holding it.
//! A file descriptor is the parcel's own copy, and each read hands out a fresh duplicate.

use std::ffi::CStr;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use jni_sys::{
    JNI_ABORT, JNIEnv, jboolean, jbyteArray, jclass, jdouble, jfloat, jint, jlong, jobject, jsize,
    jstring,
};

use super::{
    BAD_TYPE, BAD_VALUE, FDS_NOT_ALLOWED, INVALID_OPERATION, NO_MEMORY, NOT_ENOUGH_DATA,
    PERMISSION_DENIED, Parcel, Status,
};
use crate::{Method, java, register, throw};

/// A global reference, deleted when the last parcel holding it lets go.
pub struct Global(jobject);

// SAFETY: JNI global references may be used and deleted from any attached thread, and parcels are
// destroyed on whichever thread finalizes them.
unsafe impl Send for Global {}
// SAFETY: as above; the reference itself is never changed.
unsafe impl Sync for Global {}

impl Drop for Global {
    fn drop(&mut self) {
        // SAFETY: the reference is global and deleted only here.
        unsafe { java::delete_globals(&[self.0]) };
    }
}

type JavaParcel = Parcel<Arc<Global>>;

/// # Safety
/// `pointer` must be 0 or come from `nativeCreate` and not be destroyed, and no other reference
/// to the parcel may be live.
unsafe fn parcel<'a>(pointer: jlong) -> Option<&'a mut JavaParcel> {
    // SAFETY: as above.
    unsafe { (pointer as *mut JavaParcel).as_mut() }
}

const BAD_INDEX: Status = -75;
const NAME_NOT_FOUND: Status = -2;
const UNEXPECTED_NULL: Status = i32::MIN + 8;

/// `signalExceptionForError` of `android_util_Binder.cpp`, for the errors a parcel returns.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn signal(env: *mut JNIEnv, status: Status) {
    let (class, message): (&CStr, String) = match status {
        NO_MEMORY => (c"java/lang/OutOfMemoryError", String::new()),
        INVALID_OPERATION => (c"java/lang/UnsupportedOperationException", String::new()),
        BAD_VALUE | BAD_TYPE => (c"java/lang/IllegalArgumentException", String::new()),
        BAD_INDEX => (c"java/lang/IndexOutOfBoundsException", String::new()),
        NAME_NOT_FOUND => (c"java/util/NoSuchElementException", String::new()),
        PERMISSION_DENIED => (c"java/lang/SecurityException", String::new()),
        NOT_ENOUGH_DATA => (
            c"android/os/ParcelFormatException",
            "Not enough data".into(),
        ),
        FDS_NOT_ALLOWED => (
            c"java/lang/RuntimeException",
            "Not allowed to write file descriptors here".into(),
        ),
        UNEXPECTED_NULL => (c"java/lang/NullPointerException", String::new()),
        other => (
            c"java/lang/RuntimeException",
            format!("Unknown binder error code. 0x{other:x}"),
        ),
    };
    // SAFETY: as above.
    unsafe { throw(env, class, &message) };
}

/// # Safety
/// As [`signal`].
unsafe fn check(env: *mut JNIEnv, result: Result<(), Status>) {
    if let Err(status) = result {
        // SAFETY: as above.
        unsafe { signal(env, status) };
    }
}

/// A Java `int` as a size or offset; negative values are `BAD_VALUE`.
fn size(value: jint) -> Result<usize, Status> {
    usize::try_from(value).map_err(|_| BAD_VALUE)
}

/// `ensureWithinMemoryBounds`: `offset..offset + length` inside an array of `capacity`.
///
/// # Safety
/// As [`signal`].
unsafe fn within(env: *mut JNIEnv, capacity: i64, offset: jint, length: jint) -> bool {
    let fits = offset >= 0 && length >= 0 && i64::from(offset) + i64::from(length) <= capacity;
    if !fits {
        let message = format!("offset {offset} and length {length} outside {capacity} bytes");
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/ArrayIndexOutOfBoundsException", &message) };
    }
    fits
}

/// Runs `body` on the bytes of a Java array, which no JNI call may touch meanwhile.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `array` a live byte array.
unsafe fn with_array<T>(
    env: *mut JNIEnv,
    array: jbyteArray,
    mode: jint,
    body: impl FnOnce(&mut [u8]) -> T,
) -> Option<T> {
    // SAFETY: as above; the critical section ends before any other JNI call.
    unsafe {
        let jni = &**env;
        let length = (jni.v1_1.GetArrayLength)(env, array);
        let bytes = (jni.v1_2.GetPrimitiveArrayCritical)(env, array, std::ptr::null_mut());
        if bytes.is_null() {
            return None;
        }
        let slice = std::slice::from_raw_parts_mut(bytes as *mut u8, length.max(0) as usize);
        let result = body(slice);
        (jni.v1_2.ReleasePrimitiveArrayCritical)(env, array, bytes, mode);
        Some(result)
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn new_byte_array(env: *mut JNIEnv, bytes: &[u8]) -> jbyteArray {
    // SAFETY: as above; the length was checked against the parcel, which holds the bytes.
    unsafe {
        let jni = &**env;
        let array = (jni.v1_1.NewByteArray)(env, bytes.len() as jsize);
        if !array.is_null() {
            (jni.v1_1.SetByteArrayRegion)(env, array, 0, bytes.len() as jsize, bytes.as_ptr() as _);
        }
        array
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn new_string16(env: *mut JNIEnv, units: &[u16]) -> jstring {
    // SAFETY: as above.
    unsafe { ((**env).v1_1.NewString)(env, units.as_ptr(), units.len() as jsize) }
}

macro_rules! with_parcel {
    ($pointer:expr, $default:expr, |$p:ident| $body:expr) => {{
        // SAFETY: the framework passes its own `mNativePtr`, live while its Parcel is.
        match unsafe { parcel($pointer) } {
            Some($p) => $body,
            None => $default,
        }
    }};
}

unsafe extern "system" fn create(_env: *mut JNIEnv, _class: jclass) -> jlong {
    Box::into_raw(Box::new(JavaParcel::new())) as jlong
}

unsafe extern "system" fn free_buffer(_env: *mut JNIEnv, _class: jclass, pointer: jlong) {
    with_parcel!(pointer, (), |p| p.free_data())
}

unsafe extern "system" fn destroy(_env: *mut JNIEnv, _class: jclass, pointer: jlong) {
    if pointer != 0 {
        // SAFETY: the framework destroys each parcel once, then forgets its pointer.
        drop(unsafe { Box::from_raw(pointer as *mut JavaParcel) });
    }
}

unsafe extern "system" fn data_size(_env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jint {
    with_parcel!(pointer, 0, |p| p.data_size() as jint)
}

unsafe extern "system" fn data_avail(_env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jint {
    with_parcel!(pointer, 0, |p| p.data_avail() as jint)
}

unsafe extern "system" fn data_position(_env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jint {
    with_parcel!(pointer, 0, |p| p.data_position() as jint)
}

unsafe extern "system" fn data_capacity(_env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jint {
    with_parcel!(pointer, 0, |p| p.data_capacity() as jint)
}

unsafe extern "system" fn set_data_size(env: *mut JNIEnv, _class: jclass, pointer: jlong, n: jint) {
    let result = with_parcel!(pointer, Ok(()), |p| size(n)
        .and_then(|n| p.set_data_size(n)));
    // SAFETY: ART passes a valid environment.
    unsafe { check(env, result) }
}

unsafe extern "system" fn set_data_position(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    n: jint,
) {
    let result = with_parcel!(pointer, Ok(()), |p| size(n)
        .and_then(|n| p.set_data_position(n)));
    // SAFETY: as above.
    unsafe { check(env, result) }
}

unsafe extern "system" fn set_data_capacity(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    n: jint,
) {
    let result = with_parcel!(pointer, Ok(()), |p| size(n)
        .and_then(|n| p.set_data_capacity(n)));
    // SAFETY: as above.
    unsafe { check(env, result) }
}

unsafe extern "system" fn push_allow_fds(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    allow: jboolean,
) -> jboolean {
    with_parcel!(pointer, false, |p| p.push_allow_fds(allow))
}

unsafe extern "system" fn restore_allow_fds(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    previous: jboolean,
) {
    with_parcel!(pointer, (), |p| p.restore_allow_fds(previous))
}

/// The bytes of `array[offset..offset + length]`, or null, handed to `write`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `array` a byte array or null.
unsafe fn write_bytes(
    env: *mut JNIEnv,
    pointer: jlong,
    array: jbyteArray,
    offset: jint,
    length: jint,
    write: fn(&mut JavaParcel, Option<&[u8]>) -> Result<(), Status>,
) {
    with_parcel!(pointer, (), |p| {
        if array.is_null() {
            // SAFETY: as above.
            return unsafe { check(env, write(p, None)) };
        }
        // SAFETY: as above.
        let capacity = i64::from(unsafe { ((**env).v1_1.GetArrayLength)(env, array) });
        // SAFETY: as above.
        if !unsafe { within(env, capacity, offset, length) } {
            return;
        }
        let range = offset as usize..(offset + length) as usize;
        // SAFETY: as above; `write` makes no JNI call.
        let result = unsafe { with_array(env, array, JNI_ABORT, |b| write(p, Some(&b[range]))) };
        // SAFETY: as above.
        unsafe { check(env, result.unwrap_or(Err(NO_MEMORY))) }
    })
}

unsafe extern "system" fn write_byte_array(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    array: jbyteArray,
    offset: jint,
    length: jint,
) {
    // SAFETY: ART passes a valid environment and array or null.
    unsafe {
        write_bytes(
            env,
            pointer,
            array,
            offset,
            length,
            JavaParcel::write_byte_array,
        )
    }
}

unsafe extern "system" fn write_blob(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    array: jbyteArray,
    offset: jint,
    length: jint,
) {
    // SAFETY: as above.
    unsafe { write_bytes(env, pointer, array, offset, length, JavaParcel::write_blob) }
}

fn status(result: Result<(), Status>) -> jint {
    result.err().unwrap_or(0)
}

unsafe extern "system" fn write_int(_e: *mut JNIEnv, _c: jclass, pointer: jlong, v: jint) -> jint {
    with_parcel!(pointer, 0, |p| status(p.write_i32(v)))
}

unsafe extern "system" fn write_long(
    _e: *mut JNIEnv,
    _c: jclass,
    pointer: jlong,
    v: jlong,
) -> jint {
    with_parcel!(pointer, 0, |p| status(p.write_i64(v)))
}

unsafe extern "system" fn write_float(
    _e: *mut JNIEnv,
    _c: jclass,
    pointer: jlong,
    v: jfloat,
) -> jint {
    with_parcel!(pointer, 0, |p| status(p.write_f32(v)))
}

unsafe extern "system" fn write_double(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    v: jdouble,
) -> jint {
    with_parcel!(pointer, 0, |p| status(p.write_f64(v)))
}

unsafe extern "system" fn signal_exception(env: *mut JNIEnv, _class: jclass, status: jint) {
    // SAFETY: ART passes a valid environment.
    unsafe { signal(env, status) }
}

unsafe extern "system" fn write_string8(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    text: jstring,
) {
    with_parcel!(pointer, (), |p| {
        if text.is_null() {
            // SAFETY: ART passes a valid environment.
            return unsafe { check(env, p.write_string8(None)) };
        }
        // SAFETY: as above, and the characters are released after the copy.
        unsafe {
            let jni = &**env;
            let length = (jni.v1_1.GetStringUTFLength)(env, text).max(0) as usize;
            let chars = (jni.v1_1.GetStringUTFChars)(env, text, std::ptr::null_mut());
            if chars.is_null() {
                return;
            }
            let bytes = std::slice::from_raw_parts(chars.cast::<u8>(), length);
            let result = p.write_string8(Some(bytes));
            (jni.v1_1.ReleaseStringUTFChars)(env, text, chars);
            check(env, result);
        }
    })
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `text` a live string.
unsafe fn with_string16<T>(
    env: *mut JNIEnv,
    text: jstring,
    body: impl FnOnce(&[u16]) -> T,
) -> Option<T> {
    // SAFETY: as above; the critical section ends before any other JNI call.
    unsafe {
        let jni = &**env;
        let length = (jni.v1_1.GetStringLength)(env, text).max(0) as usize;
        let chars = (jni.v1_2.GetStringCritical)(env, text, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let result = body(std::slice::from_raw_parts(chars, length));
        (jni.v1_2.ReleaseStringCritical)(env, text, chars);
        Some(result)
    }
}

unsafe extern "system" fn write_string16(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    text: jstring,
) {
    with_parcel!(pointer, (), |p| {
        let result = if text.is_null() {
            p.write_string16(None)
        } else {
            // SAFETY: ART passes a valid environment and string.
            unsafe { with_string16(env, text, |units| p.write_string16(Some(units))) }
                .unwrap_or(Err(NO_MEMORY))
        };
        // SAFETY: as above.
        unsafe { check(env, result) }
    })
}

unsafe extern "system" fn write_strong_binder(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    binder: jobject,
) {
    with_parcel!(pointer, (), |p| {
        // SAFETY: ART passes a valid environment and object or null.
        let global = unsafe { java::keep(env, binder) };
        let binder = (!global.is_null()).then(|| Arc::new(Global(global)));
        // SAFETY: as above.
        unsafe { check(env, p.write_binder(binder)) }
    })
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a `FileDescriptor` or null.
unsafe fn descriptor_of(env: *mut JNIEnv, object: jobject) -> jint {
    if object.is_null() {
        return -1;
    }
    // SAFETY: as above; libcore's FileDescriptor keeps the number in `descriptor`.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let field = (jni.v1_1.GetFieldID)(env, class, c"descriptor".as_ptr(), c"I".as_ptr());
        java::delete_local(env, class);
        if field.is_null() {
            return -1;
        }
        (jni.v1_1.GetIntField)(env, object, field)
    }
}

fn duplicate(fd: jint) -> Result<OwnedFd, Status> {
    // SAFETY: F_DUPFD_CLOEXEC creates a new descriptor or fails; it never touches memory.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(-std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EBADF));
    }
    // SAFETY: `copy` is a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

unsafe extern "system" fn write_file_descriptor(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    object: jobject,
) {
    with_parcel!(pointer, (), |p| {
        // SAFETY: ART passes a valid environment and object or null.
        let fd = unsafe { descriptor_of(env, object) };
        let result = duplicate(fd).and_then(|fd| p.write_file_descriptor(fd));
        // SAFETY: as above.
        unsafe { check(env, result) }
    })
}

unsafe extern "system" fn create_byte_array(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jbyteArray {
    with_parcel!(
        pointer,
        std::ptr::null_mut(),
        |p| match p.create_byte_array() {
            // SAFETY: ART passes a valid environment.
            Some(bytes) => unsafe { new_byte_array(env, bytes) },
            None => std::ptr::null_mut(),
        }
    )
}

unsafe extern "system" fn read_byte_array(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    array: jbyteArray,
    length: jint,
) -> jboolean {
    with_parcel!(pointer, false, |p| {
        let Some(bytes) = usize::try_from(length)
            .ok()
            .and_then(|n| p.read_byte_array(n))
        else {
            return false;
        };
        // SAFETY: ART passes a valid environment and array; JNI checks the region.
        unsafe {
            let jni = &**env;
            (jni.v1_1.SetByteArrayRegion)(env, array, 0, bytes.len() as jsize, bytes.as_ptr() as _);
            !(jni.v1_2.ExceptionCheck)(env)
        }
    })
}

unsafe extern "system" fn read_blob(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jbyteArray {
    with_parcel!(pointer, std::ptr::null_mut(), |p| match p.read_blob() {
        // SAFETY: ART passes a valid environment.
        Ok(Some(bytes)) => unsafe { new_byte_array(env, bytes) },
        Ok(None) => std::ptr::null_mut(),
        Err(status) => {
            // SAFETY: as above.
            unsafe { signal(env, status) };
            std::ptr::null_mut()
        }
    })
}

unsafe extern "system" fn read_int(_e: *mut JNIEnv, _c: jclass, pointer: jlong) -> jint {
    with_parcel!(pointer, 0, |p| p.read_i32().unwrap_or(0))
}

unsafe extern "system" fn read_long(_e: *mut JNIEnv, _c: jclass, pointer: jlong) -> jlong {
    with_parcel!(pointer, 0, |p| p.read_i64().unwrap_or(0))
}

unsafe extern "system" fn read_float(_e: *mut JNIEnv, _c: jclass, pointer: jlong) -> jfloat {
    with_parcel!(pointer, 0.0, |p| p.read_f32().unwrap_or(0.0))
}

unsafe extern "system" fn read_double(_e: *mut JNIEnv, _c: jclass, pointer: jlong) -> jdouble {
    with_parcel!(pointer, 0.0, |p| p.read_f64().unwrap_or(0.0))
}

unsafe extern "system" fn read_string8(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jstring {
    with_parcel!(pointer, std::ptr::null_mut(), |p| match p.read_string8() {
        // Decoded here: NewStringUTF trusts its bytes, and these come from the parcel.
        // SAFETY: ART passes a valid environment.
        Some(bytes) => unsafe { new_string16(env, &super::utf16_from_modified_utf8(bytes)) },
        None => std::ptr::null_mut(),
    })
}

unsafe extern "system" fn read_string16(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jstring {
    with_parcel!(pointer, std::ptr::null_mut(), |p| match p.read_string16() {
        // SAFETY: ART passes a valid environment.
        Some(units) => unsafe { new_string16(env, &units) },
        None => std::ptr::null_mut(),
    })
}

unsafe extern "system" fn read_strong_binder(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jobject {
    with_parcel!(pointer, std::ptr::null_mut(), |p| match p.read_binder() {
        // SAFETY: ART passes a valid environment, and the reference is live while `binder` is.
        Some(binder) => unsafe { ((**env).v1_2.NewLocalRef)(env, binder.0) },
        None => std::ptr::null_mut(),
    })
}

unsafe extern "system" fn read_file_descriptor(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jobject {
    use std::os::fd::AsRawFd;
    with_parcel!(pointer, std::ptr::null_mut(), |p| {
        let Some(fd) = p
            .read_file_descriptor()
            .and_then(|fd| duplicate(fd.as_raw_fd()).ok())
        else {
            return std::ptr::null_mut();
        };
        // SAFETY: ART passes a valid environment; FileDescriptor is in the boot class path.
        unsafe {
            let Ok(class) = java::find_class(env, c"java/io/FileDescriptor") else {
                return std::ptr::null_mut();
            };
            let Ok(object) = java::new_object(env, class, c"()V", &[]) else {
                return std::ptr::null_mut();
            };
            let jni = &**env;
            let field = (jni.v1_1.GetFieldID)(env, class, c"descriptor".as_ptr(), c"I".as_ptr());
            if field.is_null() {
                return std::ptr::null_mut();
            }
            (jni.v1_1.SetIntField)(env, object, field, fd.into_raw_fd());
            object
        }
    })
}

unsafe extern "system" fn marshall(env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jbyteArray {
    with_parcel!(pointer, std::ptr::null_mut(), |p| match p.marshall() {
        // SAFETY: ART passes a valid environment.
        Ok(bytes) => unsafe { new_byte_array(env, &bytes) },
        Err(NO_MEMORY) => {
            // SAFETY: as above.
            unsafe {
                throw(
                    env,
                    c"java/lang/OutOfMemoryError",
                    "cannot marshall the parcel",
                )
            };
            std::ptr::null_mut()
        }
        Err(_) => {
            // SAFETY: as above.
            unsafe { throw_objects(env) };
            std::ptr::null_mut()
        }
    })
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn throw_objects(env: *mut JNIEnv) {
    let message = "Tried to marshall a Parcel that contained Binder objects.";
    // SAFETY: as above.
    unsafe { throw(env, c"java/lang/RuntimeException", message) };
}

/// Copies the marshalled data into `out`, which must hold it all; returns its size.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn marshall_into(env: *mut JNIEnv, pointer: jlong, out: Option<&mut [u8]>) -> jint {
    with_parcel!(pointer, 0, |p| {
        let bytes = match p.marshall() {
            Ok(bytes) => bytes,
            Err(NO_MEMORY) => {
                // SAFETY: as above.
                unsafe {
                    throw(
                        env,
                        c"java/lang/OutOfMemoryError",
                        "cannot marshall the parcel",
                    )
                };
                return 0;
            }
            Err(_) => {
                // SAFETY: as above.
                unsafe { throw_objects(env) };
                return 0;
            }
        };
        match out {
            Some(out) if out.len() >= bytes.len() => {
                out[..bytes.len()].copy_from_slice(&bytes);
                bytes.len() as jint
            }
            _ => {
                // SAFETY: as above.
                unsafe { throw(env, c"java/nio/BufferOverflowException", "") };
                0
            }
        }
    })
}

unsafe extern "system" fn marshall_array(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    array: jbyteArray,
    offset: jint,
    length: jint,
) -> jint {
    // SAFETY: ART passes a valid environment and array.
    unsafe {
        let capacity = i64::from(((**env).v1_1.GetArrayLength)(env, array));
        if !within(env, capacity, offset, length) {
            return 0;
        }
        // The parcel is copied out before the critical section, which then only copies bytes.
        let Some(bytes) = parcel(pointer).and_then(|p| p.marshall().ok()) else {
            return marshall_into(env, pointer, None);
        };
        let range = offset as usize..(offset + length) as usize;
        let mut written = None;
        with_array(env, array, 0, |out| {
            let out = &mut out[range];
            if out.len() >= bytes.len() {
                out[..bytes.len()].copy_from_slice(&bytes);
                written = Some(bytes.len() as jint);
            }
        });
        written.unwrap_or_else(|| {
            throw(env, c"java/nio/BufferOverflowException", "");
            0
        })
    }
}

/// The bytes of a direct buffer from `offset`, `length` long.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `buffer` a live `ByteBuffer`.
unsafe fn direct<'a>(
    env: *mut JNIEnv,
    buffer: jobject,
    offset: jint,
    length: jint,
) -> Option<&'a mut [u8]> {
    // SAFETY: as above; the buffer's memory lives as long as the Java object, which the caller holds.
    unsafe {
        let jni = &**env;
        let address = (jni.v1_4.GetDirectBufferAddress)(env, buffer) as *mut u8;
        let capacity = (jni.v1_4.GetDirectBufferCapacity)(env, buffer);
        if address.is_null() || capacity < 0 {
            throw(
                env,
                c"java/lang/IllegalArgumentException",
                "not a direct buffer",
            );
            return None;
        }
        if !within(env, capacity, offset, length) {
            return None;
        }
        Some(std::slice::from_raw_parts_mut(
            address.add(offset as usize),
            length as usize,
        ))
    }
}

unsafe extern "system" fn marshall_buffer(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    buffer: jobject,
    offset: jint,
    length: jint,
) -> jint {
    // SAFETY: ART passes a valid environment and buffer.
    unsafe {
        match direct(env, buffer, offset, length) {
            Some(out) => marshall_into(env, pointer, Some(out)),
            None => 0,
        }
    }
}

unsafe extern "system" fn unmarshall(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    array: jbyteArray,
    offset: jint,
    length: jint,
) {
    with_parcel!(pointer, (), |p| {
        if array.is_null() || length < 0 {
            return;
        }
        // SAFETY: ART passes a valid environment and array.
        unsafe {
            let capacity = i64::from(((**env).v1_1.GetArrayLength)(env, array));
            if !within(env, capacity, offset, length) {
                return;
            }
            let range = offset as usize..(offset + length) as usize;
            let result = with_array(env, array, JNI_ABORT, |b| p.unmarshall(&b[range]));
            check(env, result.unwrap_or(Err(NO_MEMORY)));
        }
    })
}

unsafe extern "system" fn unmarshall_buffer(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    buffer: jobject,
    offset: jint,
    length: jint,
) {
    with_parcel!(pointer, (), |p| {
        // SAFETY: ART passes a valid environment and buffer.
        unsafe {
            if let Some(bytes) = direct(env, buffer, offset, length) {
                check(env, p.unmarshall(bytes));
            }
        }
    })
}

/// Two parcels that may be the same one, so both are only read.
///
/// # Safety
/// As [`parcel`], for both pointers.
unsafe fn pair<'a>(a: jlong, b: jlong) -> Option<(&'a JavaParcel, &'a JavaParcel)> {
    // SAFETY: as above.
    unsafe {
        Some((
            (a as *const JavaParcel).as_ref()?,
            (b as *const JavaParcel).as_ref()?,
        ))
    }
}

unsafe extern "system" fn compare_data(
    _env: *mut JNIEnv,
    _class: jclass,
    a: jlong,
    b: jlong,
) -> jint {
    // SAFETY: both are the framework's own pointers.
    unsafe { pair(a, b) }.map_or(0, |(a, b)| a.compare_data(b))
}

unsafe extern "system" fn compare_data_in_range(
    env: *mut JNIEnv,
    _class: jclass,
    a: jlong,
    offset_a: jint,
    b: jlong,
    offset_b: jint,
    length: jint,
) -> jboolean {
    // SAFETY: as above.
    let Some((a, b)) = (unsafe { pair(a, b) }) else {
        return false;
    };
    let result = (|| a.compare_data_in_range(size(offset_a)?, b, size(offset_b)?, size(length)?))();
    result.unwrap_or_else(|status| {
        // SAFETY: ART passes a valid environment.
        unsafe { signal(env, status) };
        false
    })
}

unsafe extern "system" fn append_from(
    env: *mut JNIEnv,
    _class: jclass,
    this: jlong,
    other: jlong,
    offset: jint,
    length: jint,
) {
    if this == 0 || other == 0 {
        return;
    }
    let result = (|| {
        let (offset, length) = (size(offset)?, size(length)?);
        if this == other {
            // A parcel appended to itself reads from a copy of itself.
            // SAFETY: the framework's own pointer, and no other reference is live.
            let p = unsafe { parcel(this) }.ok_or(BAD_VALUE)?;
            let copy = p.clone();
            return p.append_from(&copy, offset, length);
        }
        // SAFETY: two different parcels of the framework.
        let (p, source) = unsafe { (parcel(this), pair(other, other)) };
        p.zip(source).map_or(Ok(()), |(p, (source, _))| {
            p.append_from(source, offset, length)
        })
    })();
    // SAFETY: ART passes a valid environment.
    unsafe { check(env, result) }
}

unsafe extern "system" fn has_fds(_env: *mut JNIEnv, _class: jclass, pointer: jlong) -> jboolean {
    with_parcel!(pointer, false, |p| p.has_file_descriptors())
}

unsafe extern "system" fn has_binders(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jboolean {
    with_parcel!(pointer, false, |p| p.has_binders())
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn in_range(
    env: *mut JNIEnv,
    pointer: jlong,
    offset: jint,
    length: jint,
    test: fn(&JavaParcel, usize, usize) -> Result<bool, Status>,
) -> jboolean {
    with_parcel!(pointer, false, |p| {
        (|| test(p, size(offset)?, size(length)?))().unwrap_or_else(|status| {
            // SAFETY: as above.
            unsafe { signal(env, status) };
            false
        })
    })
}

unsafe extern "system" fn has_fds_in_range(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    offset: jint,
    length: jint,
) -> jboolean {
    // SAFETY: ART passes a valid environment.
    unsafe {
        in_range(
            env,
            pointer,
            offset,
            length,
            JavaParcel::has_file_descriptors_in_range,
        )
    }
}

unsafe extern "system" fn has_binders_in_range(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    offset: jint,
    length: jint,
) -> jboolean {
    // SAFETY: as above.
    unsafe {
        in_range(
            env,
            pointer,
            offset,
            length,
            JavaParcel::has_binders_in_range,
        )
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `name` a string or null.
unsafe fn with_descriptor<T>(
    env: *mut JNIEnv,
    name: jstring,
    body: impl FnOnce(&[u16]) -> T,
) -> Option<T> {
    if name.is_null() {
        return Some(body(&[]));
    }
    // SAFETY: as above.
    unsafe { with_string16(env, name, body) }
}

unsafe extern "system" fn write_interface_token(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    name: jstring,
) {
    with_parcel!(pointer, (), |p| {
        // SAFETY: ART passes a valid environment and string.
        let result = unsafe { with_descriptor(env, name, |d| p.write_interface_token(d)) };
        // SAFETY: as above.
        unsafe { check(env, result.unwrap_or(Err(NO_MEMORY))) }
    })
}

unsafe extern "system" fn enforce_interface(
    env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    name: jstring,
) {
    let valid = with_parcel!(pointer, false, |p| {
        // SAFETY: ART passes a valid environment and string.
        unsafe { with_descriptor(env, name, |d| p.enforce_interface(d)) }.unwrap_or(false)
    });
    if !valid {
        let message = "Binder invocation to an incorrect interface";
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/SecurityException", message) };
    }
}

unsafe extern "system" fn replace_work_source(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
    uid: jint,
) -> jboolean {
    with_parcel!(pointer, false, |p| p.replace_calling_work_source_uid(uid))
}

unsafe extern "system" fn read_work_source(
    _env: *mut JNIEnv,
    _class: jclass,
    pointer: jlong,
) -> jint {
    with_parcel!(pointer, -1, |p| p.read_calling_work_source_uid())
}

/// No ashmem: blobs stay in the parcel.
unsafe extern "system" fn ashmem_size(_env: *mut JNIEnv, _class: jclass, _pointer: jlong) -> jlong {
    0
}

unsafe extern "system" fn alloc_size(_env: *mut JNIEnv, _class: jclass) -> jlong {
    super::global_alloc_size() as jlong
}

unsafe extern "system" fn alloc_count(_env: *mut JNIEnv, _class: jclass) -> jlong {
    super::global_alloc_count() as jlong
}

/// Sensitive data is wiped when freed on Android; this process shares nothing to wipe it from.
unsafe extern "system" fn mark_sensitive(_env: *mut JNIEnv, _class: jclass, _pointer: jlong) {}

/// Only RPC binders change a parcel's format, and there are none in one process.
unsafe extern "system" fn mark_for_binder(
    _e: *mut JNIEnv,
    _c: jclass,
    _pointer: jlong,
    _b: jobject,
) {
}

unsafe extern "system" fn is_for_rpc(
    _env: *mut JNIEnv,
    _class: jclass,
    _pointer: jlong,
) -> jboolean {
    false
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 56] = [
        (c"nativeMarkSensitive", c"(J)V", f!(mark_sensitive)),
        (
            c"nativeMarkForBinder",
            c"(JLandroid/os/IBinder;)V",
            f!(mark_for_binder),
        ),
        (c"nativeIsForRpc", c"(J)Z", f!(is_for_rpc)),
        (c"nativeDataSize", c"(J)I", f!(data_size)),
        (c"nativeDataAvail", c"(J)I", f!(data_avail)),
        (c"nativeDataPosition", c"(J)I", f!(data_position)),
        (c"nativeDataCapacity", c"(J)I", f!(data_capacity)),
        (c"nativeSetDataSize", c"(JI)V", f!(set_data_size)),
        (c"nativeSetDataPosition", c"(JI)V", f!(set_data_position)),
        (c"nativeSetDataCapacity", c"(JI)V", f!(set_data_capacity)),
        (c"nativePushAllowFds", c"(JZ)Z", f!(push_allow_fds)),
        (c"nativeRestoreAllowFds", c"(JZ)V", f!(restore_allow_fds)),
        (c"nativeWriteByteArray", c"(J[BII)V", f!(write_byte_array)),
        (c"nativeWriteBlob", c"(J[BII)V", f!(write_blob)),
        (c"nativeWriteInt", c"(JI)I", f!(write_int)),
        (c"nativeWriteLong", c"(JJ)I", f!(write_long)),
        (c"nativeWriteFloat", c"(JF)I", f!(write_float)),
        (c"nativeWriteDouble", c"(JD)I", f!(write_double)),
        (
            c"nativeSignalExceptionForError",
            c"(I)V",
            f!(signal_exception),
        ),
        (
            c"nativeWriteString8",
            c"(JLjava/lang/String;)V",
            f!(write_string8),
        ),
        (
            c"nativeWriteString16",
            c"(JLjava/lang/String;)V",
            f!(write_string16),
        ),
        (
            c"nativeWriteStrongBinder",
            c"(JLandroid/os/IBinder;)V",
            f!(write_strong_binder),
        ),
        (
            c"nativeWriteFileDescriptor",
            c"(JLjava/io/FileDescriptor;)V",
            f!(write_file_descriptor),
        ),
        (c"nativeCreateByteArray", c"(J)[B", f!(create_byte_array)),
        (c"nativeReadByteArray", c"(J[BI)Z", f!(read_byte_array)),
        (c"nativeReadBlob", c"(J)[B", f!(read_blob)),
        (c"nativeReadInt", c"(J)I", f!(read_int)),
        (c"nativeReadLong", c"(J)J", f!(read_long)),
        (c"nativeReadFloat", c"(J)F", f!(read_float)),
        (c"nativeReadDouble", c"(J)D", f!(read_double)),
        (
            c"nativeReadString8",
            c"(J)Ljava/lang/String;",
            f!(read_string8),
        ),
        (
            c"nativeReadString16",
            c"(J)Ljava/lang/String;",
            f!(read_string16),
        ),
        (
            c"nativeReadStrongBinder",
            c"(J)Landroid/os/IBinder;",
            f!(read_strong_binder),
        ),
        (
            c"nativeReadFileDescriptor",
            c"(J)Ljava/io/FileDescriptor;",
            f!(read_file_descriptor),
        ),
        (c"nativeCreate", c"()J", f!(create)),
        (c"nativeFreeBuffer", c"(J)V", f!(free_buffer)),
        (c"nativeDestroy", c"(J)V", f!(destroy)),
        (c"nativeMarshall", c"(J)[B", f!(marshall)),
        (c"nativeMarshallArray", c"(J[BII)I", f!(marshall_array)),
        (
            c"nativeMarshallBuffer",
            c"(JLjava/nio/ByteBuffer;II)I",
            f!(marshall_buffer),
        ),
        (c"nativeUnmarshall", c"(J[BII)V", f!(unmarshall)),
        (
            c"nativeUnmarshallBuffer",
            c"(JLjava/nio/ByteBuffer;II)V",
            f!(unmarshall_buffer),
        ),
        (c"nativeCompareData", c"(JJ)I", f!(compare_data)),
        (
            c"nativeCompareDataInRange",
            c"(JIJII)Z",
            f!(compare_data_in_range),
        ),
        (c"nativeAppendFrom", c"(JJII)V", f!(append_from)),
        (c"nativeHasFileDescriptors", c"(J)Z", f!(has_fds)),
        (
            c"nativeHasFileDescriptorsInRange",
            c"(JII)Z",
            f!(has_fds_in_range),
        ),
        (c"nativeHasBinders", c"(J)Z", f!(has_binders)),
        (
            c"nativeHasBindersInRange",
            c"(JII)Z",
            f!(has_binders_in_range),
        ),
        (
            c"nativeWriteInterfaceToken",
            c"(JLjava/lang/String;)V",
            f!(write_interface_token),
        ),
        (
            c"nativeEnforceInterface",
            c"(JLjava/lang/String;)V",
            f!(enforce_interface),
        ),
        (
            c"nativeReplaceCallingWorkSourceUid",
            c"(JI)Z",
            f!(replace_work_source),
        ),
        (
            c"nativeReadCallingWorkSourceUid",
            c"(J)I",
            f!(read_work_source),
        ),
        (c"nativeGetOpenAshmemSize", c"(J)J", f!(ashmem_size)),
        (c"getGlobalAllocSize", c"()J", f!(alloc_size)),
        (c"getGlobalAllocCount", c"()J", f!(alloc_count)),
    ];
    // SAFETY: each function matches its declaration in Android 16's Parcel.
    unsafe { register(env, c"android/os/Parcel", &methods) }
}
