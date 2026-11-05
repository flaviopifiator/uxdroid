//! The host's sound libraries, opened by name at run time.

use std::ffi::{CStr, c_void};

pub(crate) fn open(name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: a nul-terminated name; the library stays loaded for the process.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        Err(format!("{} is not installed", name.to_string_lossy()))
    } else {
        Ok(handle)
    }
}

pub(crate) fn symbol(handle: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: a handle from dlopen and a nul-terminated name.
    let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if address.is_null() {
        Err(format!("no {}", name.to_string_lossy()))
    } else {
        Ok(address)
    }
}

/// `name` in `handle` as a function pointer of type `F`.
///
/// # Safety
/// `F` must be a function pointer type matching the symbol's C signature.
pub(crate) unsafe fn function<F: Copy>(handle: *mut c_void, name: &CStr) -> Result<F, String> {
    const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
    let address = symbol(handle, name)?;
    // SAFETY: as above; the sizes match.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) })
}
