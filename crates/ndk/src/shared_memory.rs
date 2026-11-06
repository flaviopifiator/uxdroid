//! `ASharedMemory`, Android's ashmem regions, as Linux memfds: a region is a file descriptor that
//! maps shared, has a fixed size, and can only lose permissions.

use std::ffi::{CStr, c_char, c_int};

const F_SEAL_FUTURE_WRITE: c_int = 0x0010;

/// # Safety
/// `name` is null or a C string.
pub unsafe extern "C" fn create(name: *const c_char, size: usize) -> c_int {
    if size == 0 {
        // SAFETY: errno is this thread's.
        unsafe { *libc::__errno_location() = libc::EINVAL };
        return -1;
    }
    let name = if name.is_null() {
        c"ashmem"
    } else {
        // SAFETY: the caller passes a C string.
        unsafe { CStr::from_ptr(name) }
    };
    // memfd names are for /proc only; one longer than the kernel takes is cut, as ashmem cuts it.
    let mut short = [0 as c_char; 249];
    for (to, from) in short.iter_mut().zip(name.to_bytes().iter().take(248)) {
        *to = *from as c_char;
    }
    // SAFETY: `short` is a terminated C string.
    let fd =
        unsafe { libc::memfd_create(short.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return -1;
    }
    // SAFETY: `fd` is the memfd just made; growing it never fails for lack of memory, mapping does.
    if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0
        || unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_GROW | libc::F_SEAL_SHRINK,
            )
        } != 0
    {
        // SAFETY: as above.
        unsafe { libc::close(fd) };
        return -1;
    }
    fd
}

/// The region's size, or 0 for a descriptor that is not one, as Android answers.
pub extern "C" fn get_size(fd: c_int) -> usize {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes the whole structure on success.
    if unsafe { libc::fstat(fd, status.as_mut_ptr()) } != 0 {
        return 0;
    }
    // SAFETY: as above.
    unsafe { status.assume_init() }.st_size.max(0) as usize
}

/// Narrows what later mappings may do. Write can be taken away, never given back.
pub extern "C" fn set_prot(fd: c_int, prot: c_int) -> c_int {
    // SAFETY: fcntl on any descriptor is safe; a bad one fails.
    let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
    if seals < 0 {
        return -1;
    }
    let sealed = seals & (F_SEAL_FUTURE_WRITE | libc::F_SEAL_WRITE) != 0;
    if prot & libc::PROT_WRITE != 0 {
        return if sealed { -1 } else { 0 };
    }
    if sealed {
        return 0;
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, F_SEAL_FUTURE_WRITE) } != 0 {
        return -1;
    }
    0
}
