//! bionic functions glibc lacks, each small: fdsan with its checks off, the API level queries,
//! FORTIFY's checked forms of calls glibc has, the waits on `CLOCK_MONOTONIC` that bionic named
//! before POSIX had `clockwait`, and a few BSD stdio and memory calls.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Mutex;

use super::{API_LEVEL, fortify_failure, host_stream};

// --- fdsan ---

/// `ANDROID_FDSAN_ERROR_LEVEL_DISABLED`.
const FDSAN_DISABLED: c_int = 0;

/// The owner tag of `type` (`android_fdsan_owner_type`) and `tag`: the type in the top byte, as
/// bionic makes it. Tag 0 means no owner.
extern "C" fn fdsan_create_owner_tag(owner_type: c_int, tag: u64) -> u64 {
    if tag == 0 {
        return 0;
    }
    ((owner_type as u64 & 0xff) << 56) | (tag & ((1 << 56) - 1))
}

/// fdsan is off in uxdroid, as on a device whose app turned it off: owners are not recorded, and
/// nothing is checked or reported.
extern "C" fn fdsan_exchange_owner_tag(_fd: c_int, _expected: u64, _new_tag: u64) {}

extern "C" fn fdsan_close_with_tag(fd: c_int, _tag: u64) -> c_int {
    // SAFETY: closing a descriptor has no memory-safety preconditions.
    unsafe { libc::close(fd) }
}

/// No owner is recorded, so every descriptor has none.
extern "C" fn fdsan_get_owner_tag(_fd: c_int) -> u64 {
    0
}

extern "C" fn fdsan_get_tag_type(tag: u64) -> *const c_char {
    let name: &CStr = match tag >> 56 {
        1 => c"FILE*",
        2 => c"DIR*",
        3 => c"unique_fd",
        4 => c"sqlite",
        5 => c"FileInputStream",
        6 => c"FileOutputStream",
        7 => c"RandomAccessFile",
        8 => c"ParcelFileDescriptor",
        9 => c"ART FdFile",
        10 => c"DatagramSocketImpl",
        11 => c"SocketImpl",
        12 => c"ZipArchive",
        13 => c"native_handle_t",
        14 => c"Parcel",
        255 => c"Java object of unknown type",
        _ => c"native object of unknown type",
    };
    name.as_ptr()
}

/// The tag without its type byte, sign-extended from bit 55 as bionic does.
extern "C" fn fdsan_get_tag_value(tag: u64) -> u64 {
    (((tag << 8) as i64) >> 8) as u64
}

extern "C" fn fdsan_get_error_level() -> c_int {
    FDSAN_DISABLED
}

/// Returns the previous level, which stays disabled.
extern "C" fn fdsan_set_error_level(_level: c_int) -> c_int {
    FDSAN_DISABLED
}

// --- API levels ---

fn api_level() -> c_int {
    API_LEVEL
        .to_str()
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

extern "C" fn get_device_api_level() -> c_int {
    api_level()
}

/// The zygote sets an app's target SDK before its code runs; uxdroid starts no zygote, and the
/// device's level is the newest behavior an app can ask for.
extern "C" fn get_application_target_sdk_version() -> c_int {
    api_level()
}

// --- FORTIFY ---

unsafe extern "C" fn memchr_chk(
    text: *const c_void,
    character: c_int,
    count: usize,
    object_size: usize,
) -> *mut c_void {
    if count > object_size {
        fortify_failure("memchr");
    }
    // SAFETY: the check proves the search stays inside the caller's object.
    unsafe { libc::memchr(text, character, count) }
}

unsafe extern "C" fn memrchr_chk(
    text: *const c_void,
    character: c_int,
    count: usize,
    object_size: usize,
) -> *mut c_void {
    if count > object_size {
        fortify_failure("memrchr");
    }
    // SAFETY: as above.
    unsafe { libc::memrchr(text, character, count) }
}

unsafe extern "C" fn sendto_chk(
    socket: c_int,
    buffer: *const c_void,
    count: usize,
    object_size: usize,
    flags: c_int,
    address: *const libc::sockaddr,
    address_size: libc::socklen_t,
) -> isize {
    if count > object_size {
        fortify_failure("sendto");
    }
    // SAFETY: the check proves the send stays inside the caller's object; the rest is the
    // caller's, as for sendto.
    unsafe { libc::sendto(socket, buffer, count, flags, address, address_size) }
}

/// `__ppoll_chk` and `__ppoll64_chk`: `sigset_t` and `sigset64_t` are one 64-bit word on LP64.
unsafe extern "C" fn ppoll_chk(
    fds: *mut libc::pollfd,
    count: libc::nfds_t,
    timeout: *const libc::timespec,
    set: *const u64,
    fds_size: usize,
) -> c_int {
    if fds_size / size_of::<libc::pollfd>() < count as usize {
        fortify_failure("ppoll");
    }
    // SAFETY: the check proves the array holds `count` entries; the rest is ppoll's contract.
    unsafe { super::ppoll(fds, count, timeout, set) }
}

// --- Waits on CLOCK_MONOTONIC ---

unsafe extern "C" {
    fn pthread_cond_clockwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        clock: libc::clockid_t,
        time: *const libc::timespec,
    ) -> c_int;
    fn pthread_mutex_clocklock(
        mutex: *mut libc::pthread_mutex_t,
        clock: libc::clockid_t,
        time: *const libc::timespec,
    ) -> c_int;
    fn pthread_rwlock_clockrdlock(
        lock: *mut libc::pthread_rwlock_t,
        clock: libc::clockid_t,
        time: *const libc::timespec,
    ) -> c_int;
    fn pthread_rwlock_clockwrlock(
        lock: *mut libc::pthread_rwlock_t,
        clock: libc::clockid_t,
        time: *const libc::timespec,
    ) -> c_int;
    fn sem_clockwait(
        sem: *mut libc::sem_t,
        clock: libc::clockid_t,
        time: *const libc::timespec,
    ) -> c_int;
}

unsafe extern "C" fn cond_clockwait(
    cond: *mut c_void,
    mutex: *mut libc::pthread_mutex_t,
    clock: libc::clockid_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: `host_cond` gives glibc an object of its own alignment; the rest is the caller's.
    unsafe { pthread_cond_clockwait(super::host_cond(cond), mutex, clock, time) }
}

unsafe extern "C" fn cond_timedwait_monotonic(
    cond: *mut c_void,
    mutex: *mut libc::pthread_mutex_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { cond_clockwait(cond, mutex, libc::CLOCK_MONOTONIC, time) }
}

unsafe extern "C" fn mutex_clocklock(
    mutex: *mut libc::pthread_mutex_t,
    clock: libc::clockid_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: glibc's lock stays inside bionic's smaller mutex, as its timedlock does.
    unsafe { pthread_mutex_clocklock(mutex, clock, time) }
}

unsafe extern "C" fn mutex_timedlock_monotonic(
    mutex: *mut libc::pthread_mutex_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { pthread_mutex_clocklock(mutex, libc::CLOCK_MONOTONIC, time) }
}

unsafe extern "C" fn rwlock_clockrdlock(
    lock: *mut libc::pthread_rwlock_t,
    clock: libc::clockid_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: both libcs' rwlocks have one layout, as the audited timedrdlock relies on.
    unsafe { pthread_rwlock_clockrdlock(lock, clock, time) }
}

unsafe extern "C" fn rwlock_timedrdlock_monotonic(
    lock: *mut libc::pthread_rwlock_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { pthread_rwlock_clockrdlock(lock, libc::CLOCK_MONOTONIC, time) }
}

unsafe extern "C" fn rwlock_clockwrlock(
    lock: *mut libc::pthread_rwlock_t,
    clock: libc::clockid_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { pthread_rwlock_clockwrlock(lock, clock, time) }
}

unsafe extern "C" fn rwlock_timedwrlock_monotonic(
    lock: *mut libc::pthread_rwlock_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { pthread_rwlock_clockwrlock(lock, libc::CLOCK_MONOTONIC, time) }
}

unsafe extern "C" fn sem_clockwait_shim(
    sem: *mut c_void,
    clock: libc::clockid_t,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: `host_sem` gives glibc an object of its own alignment; the rest is the caller's.
    unsafe { sem_clockwait(super::host_sem(sem), clock, time) }
}

unsafe extern "C" fn sem_timedwait_monotonic(
    sem: *mut c_void,
    time: *const libc::timespec,
) -> c_int {
    // SAFETY: as above.
    unsafe { sem_clockwait_shim(sem, libc::CLOCK_MONOTONIC, time) }
}

// --- stdio, program name, memory ---

unsafe extern "C" {
    fn __fpurge(stream: *mut c_void);
    static mut __progname: *const c_char;
}

/// BSD's `fpurge` returns 0; glibc's `__fpurge` returns nothing.
unsafe extern "C" fn fpurge(stream: *mut c_void) -> c_int {
    // SAFETY: the caller passes an open stream.
    unsafe { __fpurge(host_stream(stream)) };
    0
}

/// The line buffer of each stream `fgetln` has read, by the stream's address. BSD keeps it in
/// the stream; glibc's has no room, so it lives here, reused by the next `fgetln` on that stream.
static LINES: Mutex<Option<HashMap<usize, Line>>> = Mutex::new(None);

struct Line {
    buffer: *mut c_char,
    capacity: usize,
}

// SAFETY: the buffer is malloc's, owned by the map, and only touched under its lock.
unsafe impl Send for Line {}

/// BSD's `fgetln`: the next line with its newline, not terminated, and its length; valid until the
/// next `fgetln` on the same stream. The lock is not held while the read waits, so a stream that
/// blocks holds up no other; the buffer is taken out for the read and put back after.
unsafe extern "C" fn fgetln(stream: *mut c_void, length: *mut usize) -> *mut c_char {
    let stream = host_stream(stream);
    let lines = || {
        LINES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    };
    let mut line = lines()
        .get_or_insert_with(HashMap::new)
        .remove(&(stream as usize))
        .unwrap_or(Line {
            buffer: std::ptr::null_mut(),
            capacity: 0,
        });
    // SAFETY: the caller passes an open stream and a writable length; getline grows the buffer
    // with malloc's memory, which only this thread holds until it is put back.
    let read = unsafe { libc::getline(&mut line.buffer, &mut line.capacity, stream.cast()) };
    let buffer = line.buffer;
    // Two threads reading one stream at once is a race in BSD too; a buffer this replaces is
    // left allocated (`Line` frees nothing) rather than freed under the other thread's line.
    lines()
        .get_or_insert_with(HashMap::new)
        .insert(stream as usize, line);
    // SAFETY: the caller passes a writable length.
    unsafe { *length = read.max(0) as usize };
    match read > 0 {
        true => buffer,
        false => std::ptr::null_mut(),
    }
}

/// Sets the name `getprogname` returns: what follows the last slash, as bionic does.
unsafe extern "C" fn setprogname(name: *const c_char) {
    // SAFETY: the caller passes a C string that outlives its use as the name, as bionic requires;
    // glibc's `__progname` is a plain pointer variable.
    unsafe {
        let slash = libc::strrchr(name, c_int::from(b'/'));
        __progname = if slash.is_null() { name } else { slash.add(1) };
    }
}

/// C23's sized frees: the size is a hint glibc's allocator does not need.
unsafe extern "C" fn free_sized(pointer: *mut c_void, _size: usize) {
    // SAFETY: the caller passes memory from malloc or null.
    unsafe { libc::free(pointer) }
}

unsafe extern "C" fn free_aligned_sized(pointer: *mut c_void, _alignment: usize, _size: usize) {
    // SAFETY: the caller passes memory from aligned_alloc or null.
    unsafe { libc::free(pointer) }
}

pub(super) const FUNCTIONS: &[(&str, *const ())] = &[
    (
        "android_fdsan_create_owner_tag",
        fdsan_create_owner_tag as *const (),
    ),
    (
        "android_fdsan_exchange_owner_tag",
        fdsan_exchange_owner_tag as *const (),
    ),
    (
        "android_fdsan_close_with_tag",
        fdsan_close_with_tag as *const (),
    ),
    (
        "android_fdsan_get_owner_tag",
        fdsan_get_owner_tag as *const (),
    ),
    (
        "android_fdsan_get_tag_type",
        fdsan_get_tag_type as *const (),
    ),
    (
        "android_fdsan_get_tag_value",
        fdsan_get_tag_value as *const (),
    ),
    (
        "android_fdsan_get_error_level",
        fdsan_get_error_level as *const (),
    ),
    (
        "android_fdsan_set_error_level",
        fdsan_set_error_level as *const (),
    ),
    (
        "android_fdsan_set_error_level_from_property",
        fdsan_set_error_level as *const (),
    ),
    (
        "android_get_device_api_level",
        get_device_api_level as *const (),
    ),
    (
        "android_get_application_target_sdk_version",
        get_application_target_sdk_version as *const (),
    ),
    ("__memchr_chk", memchr_chk as *const ()),
    ("__memrchr_chk", memrchr_chk as *const ()),
    ("__sendto_chk", sendto_chk as *const ()),
    ("__ppoll_chk", ppoll_chk as *const ()),
    ("__ppoll64_chk", ppoll_chk as *const ()),
    ("pthread_cond_clockwait", cond_clockwait as *const ()),
    (
        "pthread_cond_timedwait_monotonic_np",
        cond_timedwait_monotonic as *const (),
    ),
    ("pthread_mutex_clocklock", mutex_clocklock as *const ()),
    (
        "pthread_mutex_timedlock_monotonic_np",
        mutex_timedlock_monotonic as *const (),
    ),
    (
        "pthread_rwlock_clockrdlock",
        rwlock_clockrdlock as *const (),
    ),
    (
        "pthread_rwlock_timedrdlock_monotonic_np",
        rwlock_timedrdlock_monotonic as *const (),
    ),
    (
        "pthread_rwlock_clockwrlock",
        rwlock_clockwrlock as *const (),
    ),
    (
        "pthread_rwlock_timedwrlock_monotonic_np",
        rwlock_timedwrlock_monotonic as *const (),
    ),
    ("sem_clockwait", sem_clockwait_shim as *const ()),
    (
        "sem_timedwait_monotonic_np",
        sem_timedwait_monotonic as *const (),
    ),
    ("fpurge", fpurge as *const ()),
    ("fgetln", fgetln as *const ()),
    ("setprogname", setprogname as *const ()),
    ("free_sized", free_sized as *const ()),
    ("free_aligned_sized", free_aligned_sized as *const ()),
];
