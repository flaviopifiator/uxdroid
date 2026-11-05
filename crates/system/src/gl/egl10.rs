//! `com.google.android.gles_jni.EGLImpl`, the EGL10 of `javax.microedition.khronos.egl` that
//! `GLSurfaceView` uses. Its Java half wraps each EGL handle in an object holding it as a long and
//! finds a SurfaceView's or SurfaceHolder's `Surface`; this half reads the handles back and calls
//! the same EGL app native code gets, from `uxdroid_ndk::host_gl` or Mesa.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use jni_sys::{
    JNIEnv, jboolean, jclass, jfieldID, jint, jintArray, jlong, jmethodID, jobject, jobjectArray,
    jstring, jvalue,
};
use uxdroid_ndk::host_gl;

use super::jni::{Method, throw};

const EGL_NONE: i32 = 0x3038;
type Display = *mut c_void;

/// How many times eglInitialize ran without its eglTerminate, which `EGLImpl.getInitCount` reports.
static INITIALIZED: AtomicI32 = AtomicI32::new(0);

/// Mesa's EGL function `name`, looked up once.
fn mesa<F: Copy>(address: &OnceLock<usize>, name: &CStr) -> Option<F> {
    const { assert!(size_of::<F>() == size_of::<usize>()) };
    let found = *address.get_or_init(|| host_gl::egl_symbol(name).map_or(0, |a| a as usize));
    // SAFETY: a function pointer has the size of usize; the caller names its signature.
    (found != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&found) })
}

macro_rules! egl {
    ($name:literal: $type:ty) => {{
        static ADDRESS: OnceLock<usize> = OnceLock::new();
        mesa::<$type>(&ADDRESS, $name)
    }};
}

/// The long a handle object of `class` holds in `field`, 0 for null.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` an instance of `class` or null.
unsafe fn handle(
    env: *mut JNIEnv,
    object: jobject,
    class: &CStr,
    field: &CStr,
    id: &OnceLock<usize>,
) -> usize {
    if object.is_null() {
        return 0;
    }
    let id = *id.get_or_init(|| {
        // SAFETY: as above; the Impl classes are the framework's, with these long fields.
        unsafe {
            let found = ((**env).v1_1.FindClass)(env, class.as_ptr());
            if found.is_null() {
                ((**env).v1_1.ExceptionClear)(env);
                return 0;
            }
            ((**env).v1_1.GetFieldID)(env, found, field.as_ptr(), c"J".as_ptr()) as usize
        }
    });
    if id == 0 {
        return 0;
    }
    // SAFETY: as above.
    unsafe { ((**env).v1_1.GetLongField)(env, object, id as jfieldID) as usize }
}

macro_rules! handle_of {
    ($fn:ident, $class:literal, $field:literal) => {
        /// # Safety
        /// `env` must be the calling thread's JNI environment.
        unsafe fn $fn(env: *mut JNIEnv, object: jobject) -> *mut c_void {
            static ID: OnceLock<usize> = OnceLock::new();
            // SAFETY: as above.
            unsafe { handle(env, object, $class, $field, &ID) as *mut c_void }
        }
    };
}

handle_of!(
    display_of,
    c"com/google/android/gles_jni/EGLDisplayImpl",
    c"mEGLDisplay"
);
handle_of!(
    context_of,
    c"com/google/android/gles_jni/EGLContextImpl",
    c"mEGLContext"
);
handle_of!(
    surface_of,
    c"com/google/android/gles_jni/EGLSurfaceImpl",
    c"mEGLSurface"
);
handle_of!(
    config_of,
    c"com/google/android/gles_jni/EGLConfigImpl",
    c"mEGLConfig"
);

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn illegal(env: *mut JNIEnv, message: &str) {
    // SAFETY: as above.
    unsafe { throw(env, c"java/lang/IllegalArgumentException", message) };
}

/// An attribute list from Java, which must end with EGL_NONE; null is an empty one.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `list` an int[] or null.
unsafe fn attributes(env: *mut JNIEnv, list: jintArray) -> Option<Vec<i32>> {
    if list.is_null() {
        return Some(vec![EGL_NONE]);
    }
    // SAFETY: as above.
    let values = unsafe { ints(env, list) };
    if !values.contains(&EGL_NONE) {
        // SAFETY: as above.
        unsafe { illegal(env, "attrib_list must contain EGL_NONE!") };
        return None;
    }
    Some(values)
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `array` an int[].
unsafe fn ints(env: *mut JNIEnv, array: jintArray) -> Vec<i32> {
    // SAFETY: as above.
    unsafe {
        let length = ((**env).v1_1.GetArrayLength)(env, array).max(0);
        let mut values = vec![0; length as usize];
        ((**env).v1_1.GetIntArrayRegion)(env, array, 0, length, values.as_mut_ptr());
        values
    }
}

/// Writes `values` to the start of `array`, which must hold them, or throws.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `array` an int[] or null.
unsafe fn write_ints(env: *mut JNIEnv, array: jintArray, values: &[i32], name: &str) -> bool {
    if array.is_null() {
        // SAFETY: as above.
        unsafe { illegal(env, &format!("{name} == null")) };
        return false;
    }
    // SAFETY: as above.
    unsafe {
        if (((**env).v1_1.GetArrayLength)(env, array) as usize) < values.len() {
            illegal(env, &format!("length - offset < {}", values.len()));
            return false;
        }
        ((**env).v1_1.SetIntArrayRegion)(env, array, 0, values.len() as jint, values.as_ptr());
    }
    true
}

unsafe extern "system" fn class_init(_env: *mut JNIEnv, _class: jclass) {}

unsafe extern "system" fn get_display(
    _env: *mut JNIEnv,
    _this: jobject,
    _native: jobject,
) -> jlong {
    // SAFETY: Android has one display, which ignores its native argument.
    unsafe { host_gl::get_display(std::ptr::null_mut()) as jlong }
}

unsafe extern "system" fn initialize(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    version: jintArray,
) -> jboolean {
    let Some(f) = egl!(c"eglInitialize": unsafe extern "C" fn(Display, *mut i32, *mut i32) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe {
        if !version.is_null() && ((**env).v1_1.GetArrayLength)(env, version) < 2 {
            illegal(env, "length - offset < 2");
            return false;
        }
        let (mut major, mut minor) = (0, 0);
        let done = f(display_of(env, display), &mut major, &mut minor) != 0;
        if done {
            INITIALIZED.fetch_add(1, Ordering::Relaxed);
            if !version.is_null() {
                write_ints(env, version, &[major, minor], "major_minor");
            }
        }
        done
    }
}

unsafe extern "system" fn terminate(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
) -> jboolean {
    let Some(f) = egl!(c"eglTerminate": unsafe extern "C" fn(Display) -> u32) else {
        return false;
    };
    // SAFETY: the caller's environment.
    let done = unsafe { f(display_of(env, display)) } != 0;
    if done && INITIALIZED.load(Ordering::Relaxed) > 0 {
        INITIALIZED.fetch_sub(1, Ordering::Relaxed);
    }
    done
}

unsafe extern "system" fn init_count(_env: *mut JNIEnv, _class: jclass, _display: jobject) -> jint {
    INITIALIZED.load(Ordering::Relaxed)
}

/// Wraps each config handle in a new EGLConfigImpl and stores it in `configs`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `configs` an EGLConfig[] of at least
/// `found.len()` elements.
unsafe fn store_configs(env: *mut JNIEnv, configs: jobjectArray, found: &[*mut c_void]) -> bool {
    static CLASS: OnceLock<(usize, usize)> = OnceLock::new();
    // SAFETY: as above.
    unsafe {
        let &(class, constructor) = CLASS.get_or_init(|| {
            let class = ((**env).v1_1.FindClass)(
                env,
                c"com/google/android/gles_jni/EGLConfigImpl".as_ptr(),
            );
            if class.is_null() {
                ((**env).v1_1.ExceptionClear)(env);
                return (0, 0);
            }
            let constructor =
                ((**env).v1_1.GetMethodID)(env, class, c"<init>".as_ptr(), c"(J)V".as_ptr());
            (
                ((**env).v1_1.NewGlobalRef)(env, class) as usize,
                constructor as usize,
            )
        });
        if class == 0 || constructor == 0 {
            return false;
        }
        for (index, config) in found.iter().enumerate() {
            let argument = jvalue {
                j: *config as jlong,
            };
            let object = ((**env).v1_1.NewObjectA)(
                env,
                class as jclass,
                constructor as jmethodID,
                &argument,
            );
            if object.is_null() {
                return false;
            }
            ((**env).v1_1.SetObjectArrayElement)(env, configs, index as jint, object);
            ((**env).v1_1.DeleteLocalRef)(env, object);
        }
    }
    true
}

/// # Safety
/// `env` must be the calling thread's JNI environment; `configs` an EGLConfig[] or null.
unsafe fn configs_room(env: *mut JNIEnv, configs: jobjectArray, size: jint) -> Option<usize> {
    if configs.is_null() {
        return Some(0);
    }
    if size < 0 {
        // SAFETY: as above.
        unsafe { illegal(env, "config_size < 0") };
        return None;
    }
    // SAFETY: as above.
    let length = unsafe { ((**env).v1_1.GetArrayLength)(env, configs) };
    if length < size {
        // SAFETY: as above.
        unsafe { illegal(env, "length - offset < config_size") };
        return None;
    }
    Some(size as usize)
}

unsafe extern "system" fn choose_config(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    list: jintArray,
    configs: jobjectArray,
    size: jint,
    count: jintArray,
) -> jboolean {
    // SAFETY: the caller's environment.
    unsafe {
        let Some(list) = attributes(env, list) else {
            return false;
        };
        let Some(room) = configs_room(env, configs, size) else {
            return false;
        };
        let mut found = vec![std::ptr::null_mut(); room];
        let mut number = 0;
        let output = if configs.is_null() {
            std::ptr::null_mut()
        } else {
            found.as_mut_ptr()
        };
        if host_gl::choose_config(
            display_of(env, display),
            list.as_ptr(),
            output,
            room as i32,
            &mut number,
        ) == 0
        {
            return false;
        }
        if !count.is_null() && !write_ints(env, count, &[number], "num_config") {
            return false;
        }
        let stored = (number.max(0) as usize).min(room);
        configs.is_null() || store_configs(env, configs, &found[..stored])
    }
}

unsafe extern "system" fn get_configs(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    configs: jobjectArray,
    size: jint,
    count: jintArray,
) -> jboolean {
    let Some(f) = egl!(c"eglGetConfigs": unsafe extern "C" fn(Display, *mut *mut c_void, i32, *mut i32) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe {
        let Some(room) = configs_room(env, configs, size) else {
            return false;
        };
        let mut found = vec![std::ptr::null_mut(); room];
        let mut number = 0;
        let output = if configs.is_null() {
            std::ptr::null_mut()
        } else {
            found.as_mut_ptr()
        };
        if f(display_of(env, display), output, room as i32, &mut number) == 0 {
            return false;
        }
        if !count.is_null() && !write_ints(env, count, &[number], "num_config") {
            return false;
        }
        let stored = (number.max(0) as usize).min(room);
        configs.is_null() || store_configs(env, configs, &found[..stored])
    }
}

unsafe extern "system" fn get_config_attrib(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    config: jobject,
    attribute: jint,
    value: jintArray,
) -> jboolean {
    // SAFETY: the caller's environment.
    unsafe {
        let mut answer = 0;
        let done = host_gl::get_config_attrib(
            display_of(env, display),
            config_of(env, config),
            attribute,
            &mut answer,
        );
        done != 0 && write_ints(env, value, &[answer], "value")
    }
}

unsafe extern "system" fn create_context(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    config: jobject,
    share: jobject,
    list: jintArray,
) -> jlong {
    let Some(f) = egl!(c"eglCreateContext": unsafe extern "C" fn(Display, *mut c_void, *mut c_void, *const i32) -> *mut c_void)
    else {
        return 0;
    };
    // SAFETY: the caller's environment.
    unsafe {
        let Some(list) = attributes(env, list) else {
            return 0;
        };
        f(
            display_of(env, display),
            config_of(env, config),
            context_of(env, share),
            list.as_ptr(),
        ) as jlong
    }
}

unsafe extern "system" fn create_pbuffer_surface(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    config: jobject,
    list: jintArray,
) -> jlong {
    let Some(f) = egl!(c"eglCreatePbufferSurface": unsafe extern "C" fn(Display, *mut c_void, *const i32) -> *mut c_void)
    else {
        return 0;
    };
    // SAFETY: the caller's environment.
    unsafe {
        let Some(list) = attributes(env, list) else {
            return 0;
        };
        f(
            display_of(env, display),
            config_of(env, config),
            list.as_ptr(),
        ) as jlong
    }
}

/// A window surface on a `Surface`, which the Java half found from a SurfaceView or SurfaceHolder.
unsafe extern "system" fn create_window_surface(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    config: jobject,
    native: jobject,
    list: jintArray,
) -> jlong {
    // SAFETY: the caller's environment; the window's reference is given back after the surface
    // took its own.
    unsafe {
        let Some(list) = attributes(env, list) else {
            return 0;
        };
        let window = host_gl::window_from_surface(env, native);
        if window.is_null() {
            illegal(
                env,
                "Make sure the SurfaceView or associated SurfaceHolder has a valid Surface",
            );
            return 0;
        }
        let surface = host_gl::create_window_surface(
            display_of(env, display),
            config_of(env, config),
            window,
            list.as_ptr(),
        );
        host_gl::release_window(window);
        surface as jlong
    }
}

unsafe extern "system" fn unsupported_surface(
    env: *mut JNIEnv,
    _this: jobject,
    _a: jobject,
    _b: jobject,
    _c: jobject,
    _d: jintArray,
) -> jlong {
    // SAFETY: the caller's environment.
    unsafe {
        throw(
            env,
            c"java/lang/UnsupportedOperationException",
            "EGLImpl: window surfaces on a SurfaceTexture are not implemented",
        )
    };
    0
}

unsafe extern "system" fn unsupported_pixmap(
    env: *mut JNIEnv,
    _this: jobject,
    _s: jobject,
    _d: jobject,
    _c: jobject,
    _p: jobject,
    _l: jintArray,
) {
    // SAFETY: the caller's environment.
    unsafe {
        throw(
            env,
            c"java/lang/UnsupportedOperationException",
            "eglCreatePixmapSurface",
        )
    };
}

unsafe extern "system" fn make_current(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    draw: jobject,
    read: jobject,
    context: jobject,
) -> jboolean {
    let Some(f) = egl!(c"eglMakeCurrent": unsafe extern "C" fn(Display, *mut c_void, *mut c_void, *mut c_void) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe {
        f(
            display_of(env, display),
            surface_of(env, draw),
            surface_of(env, read),
            context_of(env, context),
        ) != 0
    }
}

unsafe extern "system" fn destroy_context(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    context: jobject,
) -> jboolean {
    let Some(f) = egl!(c"eglDestroyContext": unsafe extern "C" fn(Display, *mut c_void) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe { f(display_of(env, display), context_of(env, context)) != 0 }
}

unsafe extern "system" fn destroy_surface(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    surface: jobject,
) -> jboolean {
    // SAFETY: the caller's environment.
    unsafe { host_gl::destroy_surface(display_of(env, display), surface_of(env, surface)) != 0 }
}

unsafe extern "system" fn swap_buffers(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    surface: jobject,
) -> jboolean {
    // SAFETY: the caller's environment.
    unsafe { host_gl::swap_buffers(display_of(env, display), surface_of(env, surface)) != 0 }
}

unsafe extern "system" fn query_context(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    context: jobject,
    attribute: jint,
    value: jintArray,
) -> jboolean {
    let Some(f) =
        egl!(c"eglQueryContext": unsafe extern "C" fn(Display, *mut c_void, i32, *mut i32) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe {
        let mut answer = 0;
        f(
            display_of(env, display),
            context_of(env, context),
            attribute,
            &mut answer,
        ) != 0
            && write_ints(env, value, &[answer], "value")
    }
}

unsafe extern "system" fn query_surface(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    surface: jobject,
    attribute: jint,
    value: jintArray,
) -> jboolean {
    let Some(f) =
        egl!(c"eglQuerySurface": unsafe extern "C" fn(Display, *mut c_void, i32, *mut i32) -> u32)
    else {
        return false;
    };
    // SAFETY: the caller's environment.
    unsafe {
        let mut answer = 0;
        f(
            display_of(env, display),
            surface_of(env, surface),
            attribute,
            &mut answer,
        ) != 0
            && write_ints(env, value, &[answer], "value")
    }
}

unsafe extern "system" fn query_string(
    env: *mut JNIEnv,
    _this: jobject,
    display: jobject,
    name: jint,
) -> jstring {
    let Some(f) = egl!(c"eglQueryString": unsafe extern "C" fn(Display, i32) -> *const c_char)
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's environment; EGL returns a C string or null.
    unsafe {
        let text = f(display_of(env, display), name);
        if text.is_null() {
            return std::ptr::null_mut();
        }
        crate::new_string(env, &CStr::from_ptr(text).to_string_lossy())
    }
}

unsafe extern "system" fn get_error(_env: *mut JNIEnv, _this: jobject) -> jint {
    egl!(c"eglGetError": unsafe extern "C" fn() -> i32).map_or(0x3001, |f| {
        // SAFETY: Mesa's function, no arguments.
        unsafe { f() }
    })
}

unsafe extern "system" fn current_context(_env: *mut JNIEnv, _this: jobject) -> jlong {
    // SAFETY: Mesa's function, no arguments.
    egl!(c"eglGetCurrentContext": unsafe extern "C" fn() -> *mut c_void)
        .map_or(0, |f| unsafe { f() } as jlong)
}

unsafe extern "system" fn current_display(_env: *mut JNIEnv, _this: jobject) -> jlong {
    // SAFETY: Mesa's function, no arguments.
    egl!(c"eglGetCurrentDisplay": unsafe extern "C" fn() -> *mut c_void)
        .map_or(0, |f| unsafe { f() } as jlong)
}

unsafe extern "system" fn current_surface(_env: *mut JNIEnv, _this: jobject, which: jint) -> jlong {
    // SAFETY: Mesa's function with EGL_DRAW or EGL_READ.
    egl!(c"eglGetCurrentSurface": unsafe extern "C" fn(i32) -> *mut c_void)
        .map_or(0, |f| unsafe { f(which) } as jlong)
}

unsafe extern "system" fn release_thread(_env: *mut JNIEnv, _this: jobject) -> jboolean {
    // SAFETY: Mesa's function, no arguments.
    egl!(c"eglReleaseThread": unsafe extern "C" fn() -> u32).is_some_and(|f| unsafe { f() } != 0)
}

unsafe extern "system" fn wait_gl(_env: *mut JNIEnv, _this: jobject) -> jboolean {
    // SAFETY: Mesa's function, no arguments.
    egl!(c"eglWaitGL": unsafe extern "C" fn() -> u32).is_some_and(|f| unsafe { f() } != 0)
}

unsafe extern "system" fn wait_native(
    _env: *mut JNIEnv,
    _this: jobject,
    engine: jint,
    _bind_target: jobject,
) -> jboolean {
    // SAFETY: Mesa's function with the engine.
    egl!(c"eglWaitNative": unsafe extern "C" fn(i32) -> u32)
        .is_some_and(|f| unsafe { f(engine) } != 0)
}

/// A window has no pixmaps to copy into.
unsafe extern "system" fn copy_buffers(
    env: *mut JNIEnv,
    _this: jobject,
    _d: jobject,
    _s: jobject,
    _p: jobject,
) -> jboolean {
    // SAFETY: the caller's environment.
    unsafe {
        throw(
            env,
            c"java/lang/UnsupportedOperationException",
            "eglCopyBuffers",
        )
    };
    false
}

pub(super) const METHODS: &[Method] = &[
    (c"_nativeClassInit", c"()V", class_init as *mut c_void),
    (c"_eglGetDisplay", c"(Ljava/lang/Object;)J", get_display as *mut c_void),
    (c"eglInitialize", c"(Ljavax/microedition/khronos/egl/EGLDisplay;[I)Z", initialize as *mut c_void),
    (c"eglTerminate", c"(Ljavax/microedition/khronos/egl/EGLDisplay;)Z", terminate as *mut c_void),
    (c"getInitCount", c"(Ljavax/microedition/khronos/egl/EGLDisplay;)I", init_count as *mut c_void),
    (c"eglChooseConfig", c"(Ljavax/microedition/khronos/egl/EGLDisplay;[I[Ljavax/microedition/khronos/egl/EGLConfig;I[I)Z", choose_config as *mut c_void),
    (c"eglGetConfigs", c"(Ljavax/microedition/khronos/egl/EGLDisplay;[Ljavax/microedition/khronos/egl/EGLConfig;I[I)Z", get_configs as *mut c_void),
    (c"eglGetConfigAttrib", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;I[I)Z", get_config_attrib as *mut c_void),
    (c"_eglCreateContext", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;Ljavax/microedition/khronos/egl/EGLContext;[I)J", create_context as *mut c_void),
    (c"_eglCreatePbufferSurface", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;[I)J", create_pbuffer_surface as *mut c_void),
    (c"_eglCreatePixmapSurface", c"(Ljavax/microedition/khronos/egl/EGLSurface;Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;Ljava/lang/Object;[I)V", unsupported_pixmap as *mut c_void),
    (c"_eglCreateWindowSurface", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;Ljava/lang/Object;[I)J", create_window_surface as *mut c_void),
    (c"_eglCreateWindowSurfaceTexture", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLConfig;Ljava/lang/Object;[I)J", unsupported_surface as *mut c_void),
    (c"eglMakeCurrent", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLSurface;Ljavax/microedition/khronos/egl/EGLSurface;Ljavax/microedition/khronos/egl/EGLContext;)Z", make_current as *mut c_void),
    (c"eglDestroyContext", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLContext;)Z", destroy_context as *mut c_void),
    (c"eglDestroySurface", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLSurface;)Z", destroy_surface as *mut c_void),
    (c"eglSwapBuffers", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLSurface;)Z", swap_buffers as *mut c_void),
    (c"eglQueryContext", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLContext;I[I)Z", query_context as *mut c_void),
    (c"eglQuerySurface", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLSurface;I[I)Z", query_surface as *mut c_void),
    (c"eglQueryString", c"(Ljavax/microedition/khronos/egl/EGLDisplay;I)Ljava/lang/String;", query_string as *mut c_void),
    (c"eglGetError", c"()I", get_error as *mut c_void),
    (c"_eglGetCurrentContext", c"()J", current_context as *mut c_void),
    (c"_eglGetCurrentDisplay", c"()J", current_display as *mut c_void),
    (c"_eglGetCurrentSurface", c"(I)J", current_surface as *mut c_void),
    (c"eglReleaseThread", c"()Z", release_thread as *mut c_void),
    (c"eglWaitGL", c"()Z", wait_gl as *mut c_void),
    (c"eglWaitNative", c"(ILjava/lang/Object;)Z", wait_native as *mut c_void),
    (c"eglCopyBuffers", c"(Ljavax/microedition/khronos/egl/EGLDisplay;Ljavax/microedition/khronos/egl/EGLSurface;Ljava/lang/Object;)Z", copy_buffers as *mut c_void),
];
