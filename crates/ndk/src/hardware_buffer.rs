//! `AHardwareBuffer`, as buffers in this process' memory: a CPU can lock them and see the same
//! pixels each time. No GPU can import them yet, so a library that only allocates, describes and
//! fills one, as Instagram's loads do, works; one that hands it to EGL does not.

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

const BAD_VALUE: c_int = -libc::EINVAL;
const NO_MEMORY: c_int = -libc::ENOMEM;
const CPU_READ_MASK: u64 = 0xf;
const CPU_WRITE_MASK: u64 = 0xf0;
const FORMAT_BLOB: u32 = 0x21;

/// `AHardwareBuffer_Desc`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Desc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

struct Buffer {
    desc: Desc,
    pixels: Box<[u8]>,
    references: AtomicU32,
}

/// Bytes per pixel of the formats a CPU can address simply; a blob is `width` bytes.
fn bytes_per_pixel(format: u32) -> Option<usize> {
    Some(match format {
        // R8G8B8A8_UNORM, R8G8B8X8_UNORM, R10G10B10A2_UNORM
        1 | 2 | 0x2b => 4,
        // R8G8B8_UNORM
        3 => 3,
        // R5G6B5_UNORM
        4 => 2,
        // R16G16B16A16_FLOAT
        0x16 => 8,
        // R8_UNORM, BLOB
        0x38 | FORMAT_BLOB => 1,
        _ => return None,
    })
}

/// # Safety
/// `desc` points at a description and `out` at a buffer pointer to fill.
pub unsafe extern "C" fn allocate(desc: *const Desc, out: *mut *mut c_void) -> c_int {
    if desc.is_null() || out.is_null() {
        return BAD_VALUE;
    }
    // SAFETY: the caller passes a description.
    let mut desc = unsafe { *desc };
    let Some(bytes) = bytes_per_pixel(desc.format) else {
        return BAD_VALUE;
    };
    if desc.width == 0
        || desc.height == 0
        || desc.layers == 0
        || (desc.format == FORMAT_BLOB && desc.height != 1)
    {
        return BAD_VALUE;
    }
    let Some(size) = (desc.width as usize)
        .checked_mul(desc.height as usize)
        .and_then(|pixels| pixels.checked_mul(desc.layers as usize))
        .and_then(|pixels| pixels.checked_mul(bytes))
        .filter(|&size| size <= 1 << 30)
    else {
        return NO_MEMORY;
    };
    desc.stride = desc.width;
    let buffer = Box::new(Buffer {
        desc,
        pixels: vec![0; size].into_boxed_slice(),
        references: AtomicU32::new(1),
    });
    // SAFETY: as above.
    unsafe { *out = Box::into_raw(buffer).cast() };
    0
}

/// # Safety
/// `buffer` is a live buffer from `allocate`.
pub unsafe extern "C" fn acquire(buffer: *mut c_void) {
    if !buffer.is_null() {
        // SAFETY: the caller holds a reference, so the buffer is live.
        unsafe { &*buffer.cast::<Buffer>() }
            .references
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// # Safety
/// `buffer` is a live buffer from `allocate`; the caller gives up one reference.
pub unsafe extern "C" fn release(buffer: *mut c_void) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: as in `acquire`.
    let references = &unsafe { &*buffer.cast::<Buffer>() }.references;
    if references.fetch_sub(1, Ordering::AcqRel) == 1 {
        // SAFETY: that was the last reference.
        drop(unsafe { Box::from_raw(buffer.cast::<Buffer>()) });
    }
}

/// # Safety
/// `buffer` is live and `out` points at a description to fill.
pub unsafe extern "C" fn describe(buffer: *const c_void, out: *mut Desc) {
    if !buffer.is_null() && !out.is_null() {
        // SAFETY: as above.
        unsafe { *out = (*buffer.cast::<Buffer>()).desc };
    }
}

/// # Safety
/// `buffer` is live and `out` points at a pointer to fill; `fence` is a descriptor the call owns.
pub unsafe extern "C" fn lock(
    buffer: *mut c_void,
    usage: u64,
    fence: c_int,
    _rect: *const c_void,
    out: *mut *mut c_void,
) -> c_int {
    if fence >= 0 {
        // A fence from a GPU that is not there has nothing to wait for; the lock owns it.
        // SAFETY: the caller hands the descriptor over.
        unsafe { libc::close(fence) };
    }
    if buffer.is_null() || out.is_null() || usage & (CPU_READ_MASK | CPU_WRITE_MASK) == 0 {
        return BAD_VALUE;
    }
    // SAFETY: as above; the pixels live as long as the buffer, which a lock does not outlive.
    unsafe { *out = (*buffer.cast::<Buffer>()).pixels.as_ptr().cast_mut().cast() };
    0
}

/// # Safety
/// `fence` is null or points at a descriptor to fill.
pub unsafe extern "C" fn unlock(buffer: *mut c_void, fence: *mut c_int) -> c_int {
    if buffer.is_null() {
        return BAD_VALUE;
    }
    if !fence.is_null() {
        // The CPU's writes are done when unlock returns, so there is no fence to wait on.
        // SAFETY: as above.
        unsafe { *fence = -1 };
    }
    0
}
