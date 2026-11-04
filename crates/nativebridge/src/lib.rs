//! ART's `NativeBridge` interface, served by the uxdroid loader.
//!
//! ART tries glibc's `dlopen` first and falls back to this library, which it finds through
//! `-XX:NativeBridge=`. Bionic libraries for the host's ABI share its CPU and calling convention, so
//! a trampoline is simply the function's own address and a JNI call costs nothing extra. An
//! `arm64-v8a` library runs through the translator: its trampolines are host wrappers made from the
//! shorty ART passes for each method.
//!
//! The Android system libraries apps link against, such as `libandroid.so` or `libEGL.so`, are the
//! signature tables of `uxdroid_ndk`, served to the loader before the first app library loads.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::cell::RefCell;
use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Once;

use uxdroid_bridge::elf::Machine;
use uxdroid_bridge::{Library, guest, is_guest_address, load, table};

/// Up to identifying bridged function pointers. Below it, libnativebridge warns on every native
/// method ART registers, thousands of times for the framework.
const INTERFACE_VERSION: u32 = 8;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(message: &str) {
    let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|error| *error.borrow_mut() = message);
}

/// `NativeBridgeCallbacks` from `nativebridge/native_bridge.h`, up to the version declared above.
#[repr(C)]
pub struct NativeBridgeCallbacks {
    pub version: u32,
    pub initialize: unsafe extern "C" fn(*const c_void, *const c_char, *const c_char) -> bool,
    pub load_library: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
    pub get_trampoline:
        unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, u32) -> *mut c_void,
    pub is_supported: unsafe extern "C" fn(*const c_char) -> bool,
    pub get_app_env: unsafe extern "C" fn(*const c_char) -> *const c_void,
    pub is_compatible_with: unsafe extern "C" fn(u32) -> bool,
    pub get_signal_handler: unsafe extern "C" fn(c_int) -> *const c_void,
    pub unload_library: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub get_error: unsafe extern "C" fn() -> *const c_char,
    pub is_path_supported: unsafe extern "C" fn(*const c_char) -> bool,
    pub unused_init_anonymous_namespace: unsafe extern "C" fn(*const c_char, *const c_char) -> bool,
    pub create_namespace: unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const c_char,
        u64,
        *const c_char,
        *mut c_void,
    ) -> *mut c_void,
    pub link_namespaces: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> bool,
    pub load_library_ext: unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> *mut c_void,
    pub get_vendor_namespace: unsafe extern "C" fn() -> *mut c_void,
    pub get_exported_namespace: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub pre_zygote_fork: unsafe extern "C" fn(),
    pub get_trampoline_with_jni_call_type:
        unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, u32, c_int) -> *mut c_void,
    pub get_trampoline_for_function_pointer:
        unsafe extern "C" fn(*const c_void, *const c_char, u32, c_int) -> *mut c_void,
    pub is_native_bridge_function_pointer: unsafe extern "C" fn(*const c_void) -> bool,
}

/// The symbol ART looks up by this exact name.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static NativeBridgeItf: NativeBridgeCallbacks = NativeBridgeCallbacks {
    version: INTERFACE_VERSION,
    initialize,
    load_library,
    get_trampoline,
    is_supported,
    get_app_env,
    is_compatible_with,
    get_signal_handler,
    unload_library,
    get_error,
    is_path_supported,
    unused_init_anonymous_namespace,
    create_namespace,
    link_namespaces,
    load_library_ext,
    get_vendor_namespace,
    get_exported_namespace,
    pre_zygote_fork,
    get_trampoline_with_jni_call_type,
    get_trampoline_for_function_pointer,
    is_native_bridge_function_pointer,
};

fn provide_system_libraries() {
    static PROVIDED: Once = Once::new();
    PROVIDED.call_once(|| {
        for library in uxdroid_ndk::tables() {
            table::provide(library);
        }
        uxdroid_translate::install();
    });
}

unsafe extern "C" fn initialize(
    _callbacks: *const c_void,
    _private_dir: *const c_char,
    _instruction_set: *const c_char,
) -> bool {
    provide_system_libraries();
    true
}

unsafe extern "C" fn load_library(path: *const c_char, _flag: c_int) -> *mut c_void {
    provide_system_libraries();
    // SAFETY: ART passes a C string.
    let path = Path::new(OsStr::from_bytes(
        unsafe { CStr::from_ptr(path) }.to_bytes(),
    ));
    match load(path, &[]) {
        Ok(library) => std::ptr::from_ref::<Library>(library).cast_mut().cast(),
        Err(error) => {
            set_error(&error.to_string());
            null_mut()
        }
    }
}

unsafe extern "C" fn get_trampoline(
    handle: *mut c_void,
    name: *const c_char,
    shorty: *const c_char,
    shorty_length: u32,
) -> *mut c_void {
    // SAFETY: ART passes a handle from `load_library`, which never unloads, and a C string.
    let (library, name) = unsafe { (&*handle.cast::<Library>(), CStr::from_ptr(name)) };
    let Ok(name) = name.to_str() else {
        return null_mut();
    };
    if library.machine() != Machine::X86_64 {
        // SAFETY: ART passes the method's shorty, or null for `JNI_OnLoad`.
        let shorty = unsafe { method_shorty(shorty, shorty_length) };
        return guest::jni_trampoline(library, name, shorty).unwrap_or(null_mut());
    }
    library
        .symbol(name)
        .map_or(null_mut(), <*const c_void>::cast_mut)
}

/// The shorty ART hands over. Its `java_vm_ext.cc` passes a C string with length 0, so a zero
/// length means "up to the terminator"; any other length is taken as given.
///
/// # Safety
///
/// `shorty` is null, a C string when `length` is 0, or else valid for `length` bytes, and it
/// outlives the returned slice.
unsafe fn method_shorty<'a>(shorty: *const c_char, length: u32) -> Option<&'a [u8]> {
    if shorty.is_null() {
        return None;
    }
    if length == 0 {
        // SAFETY: a C string, by the contract above.
        return Some(unsafe { CStr::from_ptr(shorty) }.to_bytes());
    }
    // SAFETY: valid for `length` bytes, by the contract above.
    Some(unsafe { std::slice::from_raw_parts(shorty.cast::<u8>(), length as usize) })
}

unsafe extern "C" fn is_supported(_path: *const c_char) -> bool {
    // Anything glibc refused comes here, and the load itself reports whether it can be handled.
    true
}

unsafe extern "C" fn get_app_env(_instruction_set: *const c_char) -> *const c_void {
    null()
}

unsafe extern "C" fn is_compatible_with(version: u32) -> bool {
    version <= INTERFACE_VERSION
}

unsafe extern "C" fn get_signal_handler(_signal: c_int) -> *const c_void {
    null()
}

unsafe extern "C" fn unload_library(_handle: *mut c_void) -> c_int {
    // Libraries stay loaded for the life of the process.
    0
}

unsafe extern "C" fn get_error() -> *const c_char {
    LAST_ERROR.with(|error| error.borrow().as_ptr())
}

unsafe extern "C" fn is_path_supported(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn unused_init_anonymous_namespace(
    _public_libraries: *const c_char,
    _anonymous_path: *const c_char,
) -> bool {
    true
}

unsafe extern "C" fn create_namespace(
    _name: *const c_char,
    _library_path: *const c_char,
    _default_library_path: *const c_char,
    _kind: u64,
    _permitted_path: *const c_char,
    _parent: *mut c_void,
) -> *mut c_void {
    // One app per process, so every namespace is the same. Only non-null matters.
    std::ptr::dangling_mut()
}

unsafe extern "C" fn link_namespaces(
    _from: *mut c_void,
    _to: *mut c_void,
    _shared_libraries: *const c_char,
) -> bool {
    true
}

unsafe extern "C" fn load_library_ext(
    path: *const c_char,
    flag: c_int,
    _namespace: *mut c_void,
) -> *mut c_void {
    // SAFETY: forwards ART's arguments under the same contract.
    unsafe { load_library(path, flag) }
}

unsafe extern "C" fn get_vendor_namespace() -> *mut c_void {
    std::ptr::dangling_mut()
}

unsafe extern "C" fn get_exported_namespace(_name: *const c_char) -> *mut c_void {
    std::ptr::dangling_mut()
}

unsafe extern "C" fn pre_zygote_fork() {}

unsafe extern "C" fn get_trampoline_with_jni_call_type(
    handle: *mut c_void,
    name: *const c_char,
    shorty: *const c_char,
    shorty_length: u32,
    _call_type: c_int,
) -> *mut c_void {
    // SAFETY: forwards ART's arguments under the same contract.
    unsafe { get_trampoline(handle, name, shorty, shorty_length) }
}

/// A function a bridged library registers itself. Host-ABI code needs no trampoline; guest code
/// gets a wrapper made from the shorty. The guest JNIEnv's RegisterNatives already hands ART
/// wrappers, so this serves function pointers ART meets some other way.
unsafe extern "C" fn get_trampoline_for_function_pointer(
    method: *const c_void,
    shorty: *const c_char,
    shorty_length: u32,
    _call_type: c_int,
) -> *mut c_void {
    // SAFETY: ART passes the method's shorty, or null.
    let shorty = unsafe { method_shorty(shorty, shorty_length) };
    function_pointer(method, is_guest_address(method as usize), shorty)
}

/// What a function pointer becomes: itself for host code, a wrapper for guest code. Guest code
/// with no shorty to wrap it by gets null and an error, never its own address, which the host
/// would run as x86-64 instructions.
fn function_pointer(method: *const c_void, is_guest: bool, shorty: Option<&[u8]>) -> *mut c_void {
    if !is_guest {
        return method.cast_mut();
    }
    let Some(shorty) = shorty else {
        set_error("a guest function pointer came without a shorty");
        return null_mut();
    };
    guest::jni_method_trampoline(method as u64, shorty).unwrap_or_else(|| {
        set_error("a guest function pointer has a shorty that cannot be carried");
        null_mut()
    })
}

/// Only functions in guest libraries need translation; with none loaded this is one atomic load.
unsafe extern "C" fn is_native_bridge_function_pointer(method: *const c_void) -> bool {
    is_guest_address(method as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_function_pointer_without_a_shorty_is_refused() {
        let method = 0x7000_1000usize as *const c_void;
        assert_eq!(function_pointer(method, false, None), method.cast_mut());
        assert!(function_pointer(method, true, None).is_null());
        // SAFETY: the error is this thread's C string.
        let error = unsafe { CStr::from_ptr(get_error()) };
        assert_eq!(
            error.to_str(),
            Ok("a guest function pointer came without a shorty")
        );
    }
}
