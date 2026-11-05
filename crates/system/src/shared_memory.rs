//! `com.android.internal.os.ApplicationSharedMemory`: memory the system shares read-only with each app,
//! holding the network time, the property nonces and the system features. On Android, `system_server`
//! creates it and passes a read-only descriptor to `bindApplication`; here the launcher does both.
//!
//! The layout is ours, since only these natives read and write it.

use std::ffi::c_void;

use jni_sys::{JNIEnv, jboolean, jclass, jint, jintArray, jlong};

use crate::{Method, register, throw};

/// Network time, in milliseconds since the epoch at elapsed realtime zero. -1 means none.
const NETWORK_TIME: usize = 0;
/// The block of property nonces that `PropertyInvalidatedCache` reads.
const NONCES: usize = 64;
const NONCE_BYTES: usize = 8192;
/// The number of system features, then the features.
const FEATURES: usize = NONCES + NONCE_BYTES;
const MAX_FEATURES: usize = 1024;
pub const SIZE: usize = FEATURES + 4 + MAX_FEATURES * 4;

const NO_NETWORK_TIME: i64 = -1;

/// Creates the memory and returns its descriptor.
pub fn create() -> std::io::Result<i32> {
    // SAFETY: the name is a valid C string; the descriptor is checked before use.
    unsafe {
        let fd = libc::memfd_create(c"application_shared_memory".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::ftruncate(fd, SIZE as libc::off_t) != 0 {
            let error = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(error);
        }
        Ok(fd)
    }
}

/// Maps the memory of `fd`; writable only when `mutable`.
pub fn map(fd: i32, mutable: bool) -> std::io::Result<*mut u8> {
    let protection = if mutable {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_READ
    };
    // SAFETY: a fresh shared mapping of SIZE bytes; the kernel checks the descriptor's access mode.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            SIZE,
            protection,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(address as *mut u8)
}

/// Opens the same memory again for reading only, so the app cannot write it even through mprotect.
pub fn read_only(fd: i32) -> std::io::Result<i32> {
    let path = std::ffi::CString::new(format!("/proc/self/fd/{fd}")).expect("no NUL in a path");
    // SAFETY: a valid path; the result is checked.
    let opened = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if opened < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(opened)
}

/// # Safety
/// `base` must be a writable mapping from [`map`].
pub unsafe fn init(base: *mut u8) {
    // SAFETY: as above; the mapping is SIZE bytes long.
    unsafe {
        std::ptr::write_bytes(base, 0, SIZE);
        set_network_time(base, NO_NETWORK_TIME);
    }
}

/// # Safety
/// `base` must be a mapping from [`map`] that outlives the reference.
unsafe fn network_time<'a>(base: *mut u8) -> &'a std::sync::atomic::AtomicI64 {
    // SAFETY: as above; the mapping is page aligned, so offset 0 is aligned for an i64.
    unsafe { std::sync::atomic::AtomicI64::from_ptr(base.add(NETWORK_TIME) as *mut i64) }
}

/// # Safety
/// `base` must be a writable mapping from [`map`].
pub unsafe fn set_network_time(base: *mut u8, millis: i64) {
    // SAFETY: as above.
    unsafe { network_time(base) }.store(millis, std::sync::atomic::Ordering::Release);
}

/// # Safety
/// `base` must be a mapping from [`map`].
pub unsafe fn get_network_time(base: *mut u8) -> i64 {
    // SAFETY: as above.
    unsafe { network_time(base) }.load(std::sync::atomic::Ordering::Acquire)
}

/// # Safety
/// `base` must be a writable mapping from [`map`].
pub unsafe fn write_features(base: *mut u8, features: &[i32]) -> Result<(), String> {
    if features.len() > MAX_FEATURES {
        return Err(format!(
            "{} system features do not fit in {MAX_FEATURES}",
            features.len()
        ));
    }
    // SAFETY: as above; the count and the features fit in SIZE, at 4-byte aligned offsets.
    unsafe {
        let count = base.add(FEATURES) as *mut i32;
        std::ptr::copy_nonoverlapping(features.as_ptr(), count.add(1), features.len());
        count.write(features.len() as i32);
    }
    Ok(())
}

/// # Safety
/// `base` must be a mapping from [`map`].
pub unsafe fn read_features(base: *mut u8) -> Vec<i32> {
    // SAFETY: as above; a count beyond the capacity is clamped, so reads stay in the mapping.
    unsafe {
        let count = base.add(FEATURES) as *const i32;
        let length = (count.read().max(0) as usize).min(MAX_FEATURES);
        std::slice::from_raw_parts(count.add(1), length).to_vec()
    }
}

unsafe fn fail(env: *mut JNIEnv, what: &str, error: impl std::fmt::Display) {
    // SAFETY: called from a native with its valid environment.
    unsafe {
        throw(
            env,
            c"java/lang/IllegalStateException",
            &format!("{what}: {error}"),
        )
    };
}

unsafe extern "system" fn native_create(env: *mut JNIEnv, _class: jclass) -> jint {
    match create() {
        Ok(fd) => fd,
        // SAFETY: ART passes a valid environment.
        Err(error) => unsafe {
            fail(env, "cannot create application shared memory", error);
            -1
        },
    }
}

unsafe extern "system" fn native_map(
    env: *mut JNIEnv,
    _class: jclass,
    fd: jint,
    mutable: jboolean,
) -> jlong {
    match map(fd, mutable) {
        Ok(base) => base as jlong,
        // SAFETY: ART passes a valid environment.
        Err(error) => unsafe {
            fail(env, "cannot map application shared memory", error);
            0
        },
    }
}

unsafe extern "system" fn native_init(_env: *mut JNIEnv, _class: jclass, base: jlong) {
    // SAFETY: Java passes the address nativeMap returned, mapped writable by create().
    unsafe { init(base as *mut u8) };
}

unsafe extern "system" fn native_unmap(_env: *mut JNIEnv, _class: jclass, base: jlong) {
    // SAFETY: Java passes the address nativeMap returned, once.
    unsafe { libc::munmap(base as *mut c_void, SIZE) };
}

unsafe extern "system" fn native_dup_read_only(env: *mut JNIEnv, _class: jclass, fd: jint) -> jint {
    match read_only(fd) {
        Ok(opened) => opened,
        // SAFETY: ART passes a valid environment.
        Err(error) => unsafe {
            throw(
                env,
                c"java/io/IOException",
                &format!("cannot reopen application shared memory: {error}"),
            );
            -1
        },
    }
}

unsafe extern "system" fn native_set_time(
    _env: *mut JNIEnv,
    _class: jclass,
    base: jlong,
    millis: jlong,
) {
    // SAFETY: Java checks the instance is mutable before calling.
    unsafe { set_network_time(base as *mut u8, millis) };
}

unsafe extern "system" fn native_get_time(_env: *mut JNIEnv, _class: jclass, base: jlong) -> jlong {
    // SAFETY: Java checks the instance is mapped before calling.
    unsafe { get_network_time(base as *mut u8) }
}

unsafe extern "system" fn native_nonces(_env: *mut JNIEnv, _class: jclass, base: jlong) -> jlong {
    base + NONCES as jlong
}

unsafe extern "system" fn native_write_features(
    env: *mut JNIEnv,
    _class: jclass,
    base: jlong,
    array: jintArray,
) {
    // SAFETY: ART passes a valid environment and array; Java checks the instance is mutable.
    unsafe {
        let jni = &**env;
        let length = (jni.v1_1.GetArrayLength)(env, array).max(0) as usize;
        let mut features = vec![0; length];
        (jni.v1_1.GetIntArrayRegion)(env, array, 0, length as jint, features.as_mut_ptr());
        if let Err(error) = write_features(base as *mut u8, &features) {
            fail(env, "cannot share the system features", error);
        }
    }
}

unsafe extern "system" fn native_read_features(
    env: *mut JNIEnv,
    _class: jclass,
    base: jlong,
) -> jintArray {
    // SAFETY: ART passes a valid environment; Java checks the instance is mapped.
    unsafe {
        let features = read_features(base as *mut u8);
        let jni = &**env;
        let array = (jni.v1_1.NewIntArray)(env, features.len() as jint);
        if !array.is_null() {
            (jni.v1_1.SetIntArrayRegion)(env, array, 0, features.len() as jint, features.as_ptr());
        }
        array
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 10] = [
        (c"nativeCreate", c"()I", native_create as *mut c_void),
        (c"nativeMap", c"(IZ)J", native_map as *mut c_void),
        (c"nativeInit", c"(J)V", native_init as *mut c_void),
        (c"nativeUnmap", c"(J)V", native_unmap as *mut c_void),
        (
            c"nativeDupAsReadOnly",
            c"(I)I",
            native_dup_read_only as *mut c_void,
        ),
        (
            c"nativeSetLatestNetworkTimeUnixEpochMillisAtZeroElapsedRealtimeMillis",
            c"(JJ)V",
            native_set_time as *mut c_void,
        ),
        (
            c"nativeGetLatestNetworkTimeUnixEpochMillisAtZeroElapsedRealtimeMillis",
            c"(J)J",
            native_get_time as *mut c_void,
        ),
        (
            c"nativeGetSystemNonceBlock",
            c"(J)J",
            native_nonces as *mut c_void,
        ),
        (
            c"nativeWriteSystemFeaturesCache",
            c"(J[I)V",
            native_write_features as *mut c_void,
        ),
        (
            c"nativeReadSystemFeaturesCache",
            c"(J)[I",
            native_read_features as *mut c_void,
        ),
    ];
    // SAFETY: the table matches the framework's declarations in Android 16.
    unsafe {
        register(
            env,
            c"com/android/internal/os/ApplicationSharedMemory",
            &methods,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(fd: i32) {
        // SAFETY: the test owns the descriptor.
        unsafe { libc::close(fd) };
    }

    #[test]
    fn the_app_reads_what_the_system_wrote_and_cannot_write_it() {
        let fd = create().unwrap();
        let system = map(fd, true).unwrap();
        // SAFETY: mappings from `map`, written only where writable.
        unsafe {
            init(system);
            assert_eq!(get_network_time(system), NO_NETWORK_TIME);
            set_network_time(system, 1_700_000_000_000);
            write_features(system, &[3, 1, 4]).unwrap();
        }
        let app_fd = read_only(fd).unwrap();
        assert!(
            map(app_fd, true).is_err(),
            "a read-only descriptor mapped writable"
        );
        let app = map(app_fd, false).unwrap();
        // SAFETY: as above.
        unsafe {
            assert_eq!(get_network_time(app), 1_700_000_000_000);
            assert_eq!(read_features(app), vec![3, 1, 4]);
        }
        close(app_fd);
        close(fd);
    }

    #[test]
    fn too_many_features_are_refused_and_a_bad_count_stays_in_bounds() {
        let fd = create().unwrap();
        let base = map(fd, true).unwrap();
        // SAFETY: a writable mapping from `map`.
        unsafe {
            init(base);
            assert!(write_features(base, &vec![0; MAX_FEATURES + 1]).is_err());
            assert!(read_features(base).is_empty());
            (base.add(FEATURES) as *mut i32).write(i32::MAX);
            assert_eq!(read_features(base).len(), MAX_FEATURES);
        }
        close(fd);
    }
}
