//! The window system below `android.view.Surface`: the natives of AOSP's `SurfaceControl`,
//! `BLASTBufferQueue` and `Surface`, with one Wayland toplevel per layer.
//!
//! Android's window manager creates a layer for each window, `ViewRootImpl` makes a buffer queue on
//! it and draws into the queue's `Surface`. Here a layer is a [`Layer`], the queue only points at
//! one, and the Surface draws in software: `lockCanvas` points the framework's own `Canvas` at a
//! `Bitmap` the size of the window, and `unlockCanvasAndPost` copies its pixels into the window's
//! shared-memory buffer and presents it. The pixels come out as `Bitmap.getPixels` documents them,
//! 0xAARRGGBB, which is Wayland's XRGB8888 with alpha ignored, whatever Skia's own byte order is.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use jni_sys::{JNIEnv, jboolean, jclass, jint, jintArray, jlong, jobject, jstring, jvalue};
use uxdroid_platform::window::Window;

use crate::handles::Handles;
use crate::java::{self, Pending};
use crate::{Method, java_string, register, throw};

/// When this library loaded, for the first-frame report.
pub static LOADED: OnceLock<Instant> = OnceLock::new();

/// What a window shows: a name, a size, and once it first draws, its window.
pub struct Layer {
    name: String,
    size: Mutex<(i32, i32)>,
    target: Mutex<Option<Target>>,
    posted: AtomicBool,
}

/// The Wayland window of a layer and the Bitmap the framework draws it into.
struct Target {
    window: Window,
    bitmap: jobject,
    colors: jintArray,
    width: i32,
    height: i32,
}

// SAFETY: the window's mapping is plain process memory that only this Target uses, its Wayland
// proxies may be used from any thread, and both Java references are global. Every use goes through
// the Layer's mutex, so only one thread touches a Target at a time.
unsafe impl Send for Target {}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: both are this Target's own global references.
        unsafe { java::delete_globals(&[self.bitmap, self.colors]) };
    }
}

impl Layer {
    fn new(name: String, width: i32, height: i32) -> Arc<Layer> {
        Arc::new(Layer {
            name,
            size: Mutex::new((width, height)),
            target: Mutex::new(None),
            posted: AtomicBool::new(false),
        })
    }

    fn size(&self) -> (i32, i32) {
        *self.size.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_size(&self, width: i32, height: i32) {
        *self.size.lock().unwrap_or_else(|p| p.into_inner()) = (width, height);
    }

    fn target(&self) -> MutexGuard<'_, Option<Target>> {
        self.target.lock().unwrap_or_else(|p| p.into_inner())
    }
}

static SURFACE_CONTROLS: Handles<Arc<Layer>> = Handles::new();
static SURFACES: Handles<Arc<Layer>> = Handles::new();
static QUEUES: Handles<Arc<Mutex<Option<Arc<Layer>>>>> = Handles::new();

/// Why drawing failed: a Java exception is already pending, or this message should become one.
enum Failure {
    Pending,
    Message(String),
}

impl From<Pending> for Failure {
    fn from(_: Pending) -> Self {
        Failure::Pending
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn report(env: *mut JNIEnv, failure: Failure) {
    if let Failure::Message(message) = failure {
        // SAFETY: as above; the class is the one the framework declares for these natives.
        unsafe {
            throw(
                env,
                c"android/view/Surface$OutOfResourcesException",
                &message,
            )
        };
    }
}

// SurfaceControl.

unsafe extern "system" fn control_create(
    env: *mut JNIEnv,
    _class: jclass,
    _session: jobject,
    name: jstring,
    width: jint,
    height: jint,
    _format: jint,
    _flags: jint,
    _parent: jlong,
    _metadata: jobject,
) -> jlong {
    // SAFETY: ART passes a valid environment and a string or null.
    let name = unsafe { java_string(env, name) }.unwrap_or_default();
    SURFACE_CONTROLS.add(Layer::new(name, width, height))
}

unsafe extern "system" fn control_copy(_env: *mut JNIEnv, _class: jclass, control: jlong) -> jlong {
    SURFACE_CONTROLS
        .get(control)
        .map_or(0, |layer| SURFACE_CONTROLS.add(layer))
}

/// What `NativeAllocationRegistry` calls, on any thread, when a SurfaceControl is released.
extern "C" fn control_free(control: *mut c_void) {
    drop(SURFACE_CONTROLS.remove(control as i64));
}

unsafe extern "system" fn control_finalizer(_env: *mut JNIEnv, _class: jclass) -> jlong {
    control_free as *const () as jlong
}

/// Layers are the same window when they share this handle, as on Android.
unsafe extern "system" fn control_handle(
    _env: *mut JNIEnv,
    _class: jclass,
    control: jlong,
) -> jlong {
    SURFACE_CONTROLS
        .get(control)
        .map_or(0, |layer| Arc::as_ptr(&layer) as jlong)
}

unsafe extern "system" fn control_buffer_size(
    _env: *mut JNIEnv,
    _class: jclass,
    control: jlong,
    width: jint,
    height: jint,
) {
    if let Some(layer) = SURFACE_CONTROLS.get(control) {
        layer.set_size(width, height);
    }
}

// BLASTBufferQueue: points at the layer the Surface draws into.

unsafe extern "system" fn queue_create(
    _env: *mut JNIEnv,
    _class: jclass,
    _name: jstring,
    _update_destination_frame: jboolean,
) -> jlong {
    QUEUES.add(Arc::new(Mutex::new(None)))
}

unsafe extern "system" fn queue_destroy(_env: *mut JNIEnv, _class: jclass, queue: jlong) {
    drop(QUEUES.remove(queue));
}

fn queue_layer(queue: jlong) -> Option<Arc<Layer>> {
    QUEUES
        .get(queue)?
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

unsafe extern "system" fn queue_update(
    _env: *mut JNIEnv,
    _class: jclass,
    queue: jlong,
    control: jlong,
    width: jlong,
    height: jlong,
    _format: jint,
) {
    let (Some(queue), Some(layer)) = (QUEUES.get(queue), SURFACE_CONTROLS.get(control)) else {
        return;
    };
    if let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) {
        layer.set_size(width, height);
    }
    *queue.lock().unwrap_or_else(|p| p.into_inner()) = Some(layer);
}

unsafe extern "system" fn queue_same_control(
    _env: *mut JNIEnv,
    _class: jclass,
    queue: jlong,
    control: jlong,
) -> jboolean {
    match (queue_layer(queue), SURFACE_CONTROLS.get(control)) {
        (Some(current), Some(other)) => Arc::ptr_eq(&current, &other),
        _ => false,
    }
}

unsafe extern "system" fn queue_surface(
    env: *mut JNIEnv,
    _class: jclass,
    queue: jlong,
    _include_handle: jboolean,
) -> jobject {
    let Some(layer) = queue_layer(queue) else {
        return std::ptr::null_mut();
    };
    let surface = SURFACES.add(layer);
    // SAFETY: ART passes a valid environment; `Surface(long)` adopts the handle.
    let made = unsafe {
        java::find_class(env, c"android/view/Surface")
            .and_then(|class| java::new_object(env, class, c"(J)V", &[jvalue { j: surface }]))
    };
    made.unwrap_or_else(|Pending| {
        drop(SURFACES.remove(surface));
        std::ptr::null_mut()
    })
}

// Surface.

fn surface_from(layer: Option<Arc<Layer>>, current: jlong) -> jlong {
    match (layer, SURFACES.get(current)) {
        (Some(layer), Some(existing)) if Arc::ptr_eq(&layer, &existing) => current,
        (Some(layer), _) => SURFACES.add(layer),
        (None, _) => 0,
    }
}

unsafe extern "system" fn surface_from_control(
    _env: *mut JNIEnv,
    _class: jclass,
    control: jlong,
) -> jlong {
    surface_from(SURFACE_CONTROLS.get(control), 0)
}

unsafe extern "system" fn surface_get_from_control(
    _env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
    control: jlong,
) -> jlong {
    surface_from(SURFACE_CONTROLS.get(control), surface)
}

unsafe extern "system" fn surface_get_from_queue(
    _env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
    queue: jlong,
) -> jlong {
    surface_from(queue_layer(queue), surface)
}

unsafe extern "system" fn surface_release(_env: *mut JNIEnv, _class: jclass, surface: jlong) {
    drop(SURFACES.remove(surface));
}

unsafe extern "system" fn surface_is_valid(
    _env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
) -> jboolean {
    SURFACES.get(surface).is_some()
}

fn drawn_size(surface: jlong) -> (i32, i32) {
    let Some(layer) = SURFACES.get(surface) else {
        return (0, 0);
    };
    let target = layer.target();
    match target.as_ref() {
        Some(target) => (target.width, target.height),
        None => layer.size(),
    }
}

unsafe extern "system" fn surface_width(_env: *mut JNIEnv, _class: jclass, surface: jlong) -> jint {
    drawn_size(surface).0
}

unsafe extern "system" fn surface_height(
    _env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
) -> jint {
    drawn_size(surface).1
}

impl Target {
    /// Opens the layer's window. The compositor may choose another size, which the Bitmap follows.
    ///
    /// # Safety
    /// `env` must be the calling thread's JNI environment.
    unsafe fn open(env: *mut JNIEnv, layer: &Layer) -> Result<Target, Failure> {
        let (width, height) = layer.size();
        let window = Window::open(&layer.name, width, height)
            .map_err(|error| Failure::Message(format!("cannot open a window: {error}")))?;
        let (width, height) = (window.width(), window.height());
        // SAFETY: as above.
        unsafe {
            let class = java::find_class(env, c"android/graphics/Bitmap")?;
            let config_class = java::find_class(env, c"android/graphics/Bitmap$Config")?;
            let argb = java::static_object_field(
                env,
                config_class,
                c"ARGB_8888",
                c"Landroid/graphics/Bitmap$Config;",
            )?;
            let bitmap = java::call_static_object(
                env,
                class,
                c"createBitmap",
                c"(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;",
                &[
                    jvalue { i: width },
                    jvalue { i: height },
                    jvalue { l: argb },
                ],
            )?;
            java::delete_local(env, argb);
            let colors = ((**env).v1_1.NewIntArray)(env, width * height);
            if colors.is_null() {
                java::delete_local(env, bitmap);
                return Err(Failure::Pending);
            }
            Ok(Target {
                window,
                bitmap: java::global(env, bitmap),
                colors: java::global(env, colors),
                width,
                height,
            })
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, `canvas` a Canvas, `dirty` a Rect or null.
unsafe fn lock(
    env: *mut JNIEnv,
    layer: &Layer,
    canvas: jobject,
    dirty: jobject,
) -> Result<(), Failure> {
    let mut target = layer.target();
    if target.is_none() {
        // SAFETY: as above.
        *target = Some(unsafe { Target::open(env, layer)? });
    }
    let Some(target) = target.as_ref() else {
        return Err(Failure::Message("no window".into()));
    };
    // SAFETY: as above; the Bitmap is mutable and lives as long as the Target.
    unsafe {
        java::call_void(
            env,
            canvas,
            c"setBitmap",
            c"(Landroid/graphics/Bitmap;)V",
            &[jvalue { l: target.bitmap }],
        )?;
        // The Bitmap keeps the last frame, but posting copies all of it, so every frame redraws all.
        if !dirty.is_null() {
            let args = [0, 0, target.width, target.height].map(|i| jvalue { i });
            java::call_void(env, dirty, c"set", c"(IIII)V", &args)?;
        }
    }
    Ok(())
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `canvas` the Canvas `lock` prepared.
unsafe fn post(env: *mut JNIEnv, layer: &Layer, canvas: jobject) -> Result<(), Failure> {
    let mut target = layer.target();
    let Some(target) = target.as_mut() else {
        return Err(Failure::Message(format!("{} was never locked", layer.name)));
    };
    let (width, height) = (target.width, target.height);
    // SAFETY: as above. The window's frame holds `width * height` pixels, the same as the array.
    unsafe {
        let args = [
            jvalue { l: target.colors },
            jvalue { i: 0 },
            jvalue { i: width },
            jvalue { i: 0 },
            jvalue { i: 0 },
            jvalue { i: width },
            jvalue { i: height },
        ];
        java::call_void(env, target.bitmap, c"getPixels", c"([IIIIIII)V", &args)?;
        let frame = target.window.frame();
        let count = frame.pixels.len().min((width * height) as usize);
        ((**env).v1_1.GetIntArrayRegion)(
            env,
            target.colors,
            0,
            count as jint,
            frame.pixels.as_mut_ptr().cast::<jint>(),
        );
        java::call_void(
            env,
            canvas,
            c"setBitmap",
            c"(Landroid/graphics/Bitmap;)V",
            &[jvalue {
                l: std::ptr::null_mut(),
            }],
        )?;
    }
    target
        .window
        .present()
        .map_err(|error| Failure::Message(format!("cannot present a frame: {error}")))?;
    if !layer.posted.swap(true, Ordering::Relaxed) {
        let since = LOADED.get().map_or(0, |start| start.elapsed().as_millis());
        eprintln!(
            "uxdroid: first frame posted on {} {width}x{height}, {since} ms after the framework started",
            layer.name
        );
    }
    Ok(())
}

unsafe extern "system" fn surface_lock_canvas(
    env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
    canvas: jobject,
    dirty: jobject,
) -> jlong {
    let Some(layer) = SURFACES.get(surface) else {
        // SAFETY: ART passes a valid environment.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalStateException",
                "the surface was released",
            )
        };
        return 0;
    };
    // SAFETY: ART passes a valid environment, a Canvas and a Rect or null.
    match unsafe { lock(env, &layer, canvas, dirty) } {
        // As on Android, the locked surface is the Surface itself with one more reference, which
        // unlockCanvasAndPost releases after posting.
        Ok(()) if SURFACES.retain(surface) => surface,
        Ok(()) => 0,
        Err(failure) => {
            // SAFETY: as above.
            unsafe { report(env, failure) };
            0
        }
    }
}

unsafe extern "system" fn surface_unlock_and_post(
    env: *mut JNIEnv,
    _class: jclass,
    surface: jlong,
    canvas: jobject,
) {
    let Some(layer) = SURFACES.get(surface) else {
        // SAFETY: ART passes a valid environment.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalStateException",
                "the surface was released",
            )
        };
        return;
    };
    // SAFETY: ART passes a valid environment and the Canvas it locked.
    if let Err(failure) = unsafe { post(env, &layer, canvas) } {
        // SAFETY: as above.
        unsafe { report(env, failure) };
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let control: [Method; 5] = [
        (
            c"nativeCreate",
            c"(Landroid/view/SurfaceSession;Ljava/lang/String;IIIIJLandroid/os/Parcel;)J",
            f!(control_create),
        ),
        (c"nativeCopyFromSurfaceControl", c"(J)J", f!(control_copy)),
        (
            c"nativeGetNativeSurfaceControlFinalizer",
            c"()J",
            f!(control_finalizer),
        ),
        (c"nativeGetHandle", c"(J)J", f!(control_handle)),
        (
            c"nativeUpdateDefaultBufferSize",
            c"(JII)V",
            f!(control_buffer_size),
        ),
    ];
    let queue: [Method; 5] = [
        (c"nativeCreate", c"(Ljava/lang/String;Z)J", f!(queue_create)),
        (c"nativeDestroy", c"(J)V", f!(queue_destroy)),
        (
            c"nativeGetSurface",
            c"(JZ)Landroid/view/Surface;",
            f!(queue_surface),
        ),
        (c"nativeUpdate", c"(JJJJI)V", f!(queue_update)),
        (
            c"nativeIsSameSurfaceControl",
            c"(JJ)Z",
            f!(queue_same_control),
        ),
    ];
    let surface: [Method; 9] = [
        (
            c"nativeCreateFromSurfaceControl",
            c"(J)J",
            f!(surface_from_control),
        ),
        (
            c"nativeGetFromSurfaceControl",
            c"(JJ)J",
            f!(surface_get_from_control),
        ),
        (
            c"nativeGetFromBlastBufferQueue",
            c"(JJ)J",
            f!(surface_get_from_queue),
        ),
        (
            c"nativeLockCanvas",
            c"(JLandroid/graphics/Canvas;Landroid/graphics/Rect;)J",
            f!(surface_lock_canvas),
        ),
        (
            c"nativeUnlockCanvasAndPost",
            c"(JLandroid/graphics/Canvas;)V",
            f!(surface_unlock_and_post),
        ),
        (c"nativeRelease", c"(J)V", f!(surface_release)),
        (c"nativeIsValid", c"(J)Z", f!(surface_is_valid)),
        (c"nativeGetWidth", c"(J)I", f!(surface_width)),
        (c"nativeGetHeight", c"(J)I", f!(surface_height)),
    ];
    // SAFETY: each table matches the framework's declarations in Android 16.
    unsafe {
        register(env, c"android/view/SurfaceControl", &control)?;
        register(env, c"android/graphics/BLASTBufferQueue", &queue)?;
        register(env, c"android/view/Surface", &surface)?;
    }
    Ok(())
}
