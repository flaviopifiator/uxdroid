//! ART's `NativeBridge` interface, served by the uxdroid loader.
//!
//! ART tries glibc's `dlopen` first and falls back to this library, which it finds through
//! `-XX:NativeBridge=`. Bionic libraries here share the host's CPU and calling convention, so a
//! trampoline is simply the function's own address and a JNI call costs nothing extra.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::cell::RefCell;
use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use uxdroid_bridge::{Library, load};

/// Includes linker namespaces, the lowest version current ART accepts.
const INTERFACE_VERSION: u32 = 3;

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
    version: u32,
    initialize: unsafe extern "C" fn(*const c_void, *const c_char, *const c_char) -> bool,
    load_library: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
    get_trampoline:
        unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, u32) -> *mut c_void,
    is_supported: unsafe extern "C" fn(*const c_char) -> bool,
    get_app_env: unsafe extern "C" fn(*const c_char) -> *const c_void,
    is_compatible_with: unsafe extern "C" fn(u32) -> bool,
    get_signal_handler: unsafe extern "C" fn(c_int) -> *const c_void,
    unload_library: unsafe extern "C" fn(*mut c_void) -> c_int,
    get_error: unsafe extern "C" fn() -> *const c_char,
    is_path_supported: unsafe extern "C" fn(*const c_char) -> bool,
    unused_init_anonymous_namespace:
        unsafe extern "C" fn(*const c_char, *const c_char) -> bool,
    create_namespace: unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const c_char,
        u64,
        *const c_char,
        *mut c_void,
    ) -> *mut c_void,
    link_namespaces: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> bool,
    load_library_ext: unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> *mut c_void,
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
};

unsafe extern "C" fn initialize(
    _callbacks: *const c_void,
    _private_dir: *const c_char,
    _instruction_set: *const c_char,
) -> bool {
    true
}

unsafe extern "C" fn load_library(path: *const c_char, _flag: c_int) -> *mut c_void {
    // SAFETY: ART passes a C string.
    let path = Path::new(OsStr::from_bytes(unsafe { CStr::from_ptr(path) }.to_bytes()));
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
    _shorty: *const c_char,
    _shorty_length: u32,
) -> *mut c_void {
    // SAFETY: ART passes a handle from `load_library`, which never unloads, and a C string.
    let (library, name) = unsafe { (&*handle.cast::<Library>(), CStr::from_ptr(name)) };
    name.to_str()
        .ok()
        .and_then(|name| library.symbol(name))
        .map_or(null_mut(), <*const c_void>::cast_mut)
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
