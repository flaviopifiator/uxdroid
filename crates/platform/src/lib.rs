//! JNI functions behind `dev.uxdroid.runtime.Window` and `android.graphics.Canvas`. As a Rust library,
//! its `Window` also shows the windows of AOSP's framework (`crates/system`).
#![cfg(target_os = "linux")]

pub mod raster;
pub mod window;

use std::ffi::{CStr, CString};

use jni_sys::{JNIEnv, jclass, jfloat, jint, jlong, jstring};

use window::Window;

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn throw(env: *mut JNIEnv, message: &str) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: the JNI table holds these functions, and the class is in the boot class path.
    unsafe {
        let table = &**env;
        let class = (table.v1_1.FindClass)(env, c"java/lang/RuntimeException".as_ptr());
        (table.v1_1.ThrowNew)(env, class, text.as_ptr());
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `text` a valid non-null string.
unsafe fn java_string(env: *mut JNIEnv, text: jstring) -> String {
    // SAFETY: as above. The characters are copied out before being released.
    unsafe {
        let table = &**env;
        let chars = (table.v1_1.GetStringUTFChars)(env, text, std::ptr::null_mut());
        if chars.is_null() {
            return String::new();
        }
        let owned = CStr::from_ptr(chars).to_string_lossy().into_owned();
        (table.v1_1.ReleaseStringUTFChars)(env, text, chars);
        owned
    }
}

/// # Safety
/// `handle` must come from `nCreate`. Windows live until the process exits.
unsafe fn window<'a>(handle: jlong) -> &'a mut Window {
    // SAFETY: as above, and the Java class calls these from one thread.
    unsafe { &mut *(handle as *mut Window) }
}

/// # Safety
/// Called by ART with valid JNI arguments.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nCreate(
    env: *mut JNIEnv,
    _class: jclass,
    title: jstring,
    width: jint,
    height: jint,
) -> jlong {
    // SAFETY: ART passes a valid environment and string.
    let title = unsafe { java_string(env, title) };
    match Window::open(&title, width, height) {
        Ok(window) => Box::into_raw(Box::new(window)) as jlong,
        Err(error) => {
            // SAFETY: ART passes a valid environment.
            unsafe { throw(env, &format!("cannot open a window: {error}")) };
            0
        }
    }
}

/// # Safety
/// Called by ART with a handle from `nCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nWidth(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jint {
    // SAFETY: as above.
    unsafe { window(handle) }.width()
}

/// # Safety
/// Called by ART with a handle from `nCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nHeight(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jint {
    // SAFETY: as above.
    unsafe { window(handle) }.height()
}

/// # Safety
/// Called by ART with a handle from `nCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nPresent(
    env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) {
    // SAFETY: as above.
    if let Err(error) = unsafe { window(handle) }.present() {
        // SAFETY: ART passes a valid environment.
        unsafe { throw(env, &format!("cannot present a frame: {error}")) };
    }
}

/// # Safety
/// Called by ART with a handle from `nCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nWaitForEvents(
    env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
) -> jint {
    // SAFETY: as above.
    match unsafe { window(handle) }.wait_for_events() {
        Ok(state) => state,
        Err(error) => {
            // SAFETY: ART passes a valid environment.
            unsafe { throw(env, &format!("lost the display: {error}")) };
            window::CLOSED
        }
    }
}

/// # Safety
/// Called by ART with a handle from `nCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nDrawRect(
    _env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
    left: jfloat,
    top: jfloat,
    right: jfloat,
    bottom: jfloat,
    color: jint,
) {
    // SAFETY: as above.
    let window = unsafe { window(handle) };
    raster::fill_rect(&mut window.frame(), left, top, right, bottom, color as u32);
}

/// # Safety
/// Called by ART with a handle from `nCreate` and a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_uxdroid_runtime_Window_nDrawText(
    env: *mut JNIEnv,
    _class: jclass,
    handle: jlong,
    text: jstring,
    x: jfloat,
    y: jfloat,
    color: jint,
    size: jfloat,
) {
    // SAFETY: as above.
    let (window, text) = unsafe { (window(handle), java_string(env, text)) };
    raster::draw_text(&mut window.frame(), &text, x, y, color as u32, size);
}
