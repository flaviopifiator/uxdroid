//! The window system below `android.view.Surface`: the natives of AOSP's `SurfaceControl`,
//! `BLASTBufferQueue` and `Surface`, with one Wayland toplevel per window.
//!
//! Android's window manager creates a layer for each window, `ViewRootImpl` makes a buffer queue on
//! it and draws into the queue's `Surface`. Here a layer is a [`Layer`], the queue only points at
//! one, and the Surface draws in software: `lockCanvas` points the framework's own `Canvas` at a
//! `Bitmap` the size of the window, and `unlockCanvasAndPost` copies its pixels into the window's
//! shared-memory buffer and presents it. The pixels come out as `Bitmap.getPixels` documents them,
//! 0xAARRGGBB, which is Wayland's XRGB8888 with alpha ignored, whatever Skia's own byte order is.
//!
//! A layer with a parent, such as a `SurfaceView`'s, opens no window: its frames are kept and
//! composed into the window of its root layer, at the position its transactions set, below the
//! window's own pixels when any layer on its way to the root has a negative z, as SurfaceView's
//! does, and above them otherwise. Transactions apply at once, not when the framework applies them.
//!
//! A root layer's window is also where its input comes from: the window manager attaches the
//! server end of the window's input channel to the layer, the Looper of the thread that first draws
//! watches the Wayland connection, and the seat events read there, or during a frame's round trip,
//! are sent down that channel.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use jni_sys::{JNIEnv, jboolean, jclass, jint, jintArray, jlong, jobject, jstring, jvalue};

use crate::compose::{Placed, compose};
use crate::handles::Handles;
use crate::input::natives::{Channel, channel_of};
use crate::java::{self, Field, Pending};
use crate::wayland::Window;
use crate::{Method, java_string, register, throw};

/// Bounds a child layer's Bitmap, which no compositor sizes.
const MAX_CHILD_DIMENSION: i32 = 16384;

/// When this library loaded, for the first-frame report.
pub static LOADED: OnceLock<Instant> = OnceLock::new();

/// What a window shows: a name, a size, its place among other layers, its last frame, and once it
/// first draws, where it draws.
pub struct Layer {
    name: String,
    size: Mutex<(i32, i32)>,
    geometry: Mutex<Geometry>,
    children: Mutex<Vec<Weak<Layer>>>,
    /// The last frame posted, read by composition. Only ever locked last, so it cannot deadlock.
    content: Mutex<Content>,
    target: Mutex<Option<Target>>,
    posted: AtomicBool,
    /// The server end of the window's input channel, for a root layer.
    input: Mutex<Option<Channel>>,
}

#[derive(Default)]
struct Geometry {
    parent: Option<Arc<Layer>>,
    x: i32,
    y: i32,
    z: i32,
    hidden: bool,
}

#[derive(Default)]
struct Content {
    pixels: Vec<u32>,
    width: i32,
    height: i32,
}

/// Where a layer draws: the Wayland window of a root layer, or nothing for a child, and the Bitmap
/// the framework draws it into.
struct Target {
    window: Option<Window>,
    bitmap: jobject,
    colors: jintArray,
    width: i32,
    height: i32,
    /// The MessageQueue watching the window's connection and the FileDescriptor it watches, global
    /// references or null.
    queue: jobject,
    descriptor: jobject,
    /// The id the queue's listener knows the layer by.
    watched: jlong,
}

// SAFETY: the window's mapping is plain process memory that only this Target uses, its Wayland
// proxies may be used from any thread, and both Java references are global. Every use goes through
// the Layer's mutex, so only one thread touches a Target at a time.
unsafe impl Send for Target {}

impl Drop for Target {
    fn drop(&mut self) {
        // The Looper must stop watching the connection before the window closes it.
        if !self.queue.is_null()
            && let Some(env) = java::attached_env()
            // SAFETY: an attached thread's environment.
            && !unsafe { ((**env).v1_2.ExceptionCheck)(env) }
        {
            let args = [jvalue { l: self.descriptor }];
            // SAFETY: as above, with this Target's own references.
            let removed = unsafe {
                java::call_void(
                    env,
                    self.queue,
                    c"removeOnFileDescriptorEventListener",
                    c"(Ljava/io/FileDescriptor;)V",
                    &args,
                )
            };
            if removed.is_err() {
                // SAFETY: as above.
                unsafe { ((**env).v1_1.ExceptionClear)(env) };
            }
        }
        drop(WATCHED.remove(self.watched));
        // SAFETY: these are this Target's own global references or null.
        unsafe { java::delete_globals(&[self.bitmap, self.colors, self.queue, self.descriptor]) };
    }
}

impl Layer {
    fn new(name: String, width: i32, height: i32) -> Arc<Layer> {
        Arc::new(Layer {
            name,
            size: Mutex::new((width, height)),
            geometry: Mutex::new(Geometry::default()),
            children: Mutex::new(Vec::new()),
            content: Mutex::new(Content::default()),
            target: Mutex::new(None),
            posted: AtomicBool::new(false),
            input: Mutex::new(None),
        })
    }

    fn geometry(&self) -> MutexGuard<'_, Geometry> {
        self.geometry.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn parent(&self) -> Option<Arc<Layer>> {
        self.geometry().parent.clone()
    }

    /// Moves this layer under `parent`, refusing a parent that is the layer or one of its children.
    fn set_parent(self: &Arc<Layer>, parent: Option<Arc<Layer>>) {
        let mut ancestor = parent.clone();
        while let Some(layer) = ancestor {
            if Arc::ptr_eq(&layer, self) {
                return;
            }
            ancestor = layer.parent();
        }
        if let Some(parent) = &parent {
            let mut children = parent.children.lock().unwrap_or_else(|p| p.into_inner());
            children.retain(|child| child.strong_count() > 0);
            children.push(Arc::downgrade(self));
        }
        if let Some(old) = std::mem::replace(&mut self.geometry().parent, parent) {
            let mut children = old.children.lock().unwrap_or_else(|p| p.into_inner());
            children.retain(|child| !std::ptr::eq(child.as_ptr(), Arc::as_ptr(self)));
        }
    }

    fn root(self: &Arc<Layer>) -> Arc<Layer> {
        let mut layer = self.clone();
        while let Some(parent) = layer.parent() {
            layer = parent;
        }
        layer
    }

    /// Every visible descendant with a frame, its position in the root, and whether it goes below.
    fn descendants(
        &self,
        x: i32,
        y: i32,
        below: bool,
        found: &mut Vec<(Arc<Layer>, i32, i32, bool)>,
    ) {
        let children: Vec<Arc<Layer>> = self
            .children
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for child in children {
            let (child_x, child_y, child_below) = {
                let geometry = child.geometry();
                if geometry.hidden {
                    continue;
                }
                (
                    x.saturating_add(geometry.x),
                    y.saturating_add(geometry.y),
                    below || geometry.z < 0,
                )
            };
            found.push((child.clone(), child_x, child_y, child_below));
            child.descendants(child_x, child_y, child_below, found);
        }
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
/// Root layers whose window a Looper watches, by the id its listener holds.
static WATCHED: Handles<Weak<Layer>> = Handles::new();
/// `dev.uxdroid.system.WindowInput`, found when the natives are registered.
static WINDOW_INPUT_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// `MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT`.
const EVENT_INPUT: jint = 1;

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
    flags: jint,
    parent: jlong,
    _metadata: jobject,
) -> jlong {
    // SAFETY: ART passes a valid environment and a string or null.
    let name = unsafe { java_string(env, name) }.unwrap_or_default();
    let layer = Layer::new(name, width, height);
    layer.geometry().hidden = flags & CREATE_HIDDEN != 0;
    if parent != 0 {
        layer.set_parent(SURFACE_CONTROLS.get(parent));
    }
    SURFACE_CONTROLS.add(layer)
}

unsafe extern "system" fn control_copy(_env: *mut JNIEnv, _class: jclass, control: jlong) -> jlong {
    SURFACE_CONTROLS
        .get(control)
        .map_or(0, |layer| SURFACE_CONTROLS.add(layer))
}

/// The `hidden` flag of `SurfaceControl.Builder`, and of a transaction's `setFlags`.
const CREATE_HIDDEN: jint = 0x4;
const SURFACE_HIDDEN: jint = 0x1;

unsafe extern "system" fn control_reparent(
    _env: *mut JNIEnv,
    _class: jclass,
    _transaction: jlong,
    control: jlong,
    parent: jlong,
) {
    if let Some(layer) = SURFACE_CONTROLS.get(control) {
        layer.set_parent(SURFACE_CONTROLS.get(parent));
    }
}

unsafe extern "system" fn control_position(
    _env: *mut JNIEnv,
    _class: jclass,
    _transaction: jlong,
    control: jlong,
    x: f32,
    y: f32,
) {
    if let Some(layer) = SURFACE_CONTROLS.get(control) {
        let mut geometry = layer.geometry();
        // `as` saturates, and NaN becomes 0.
        (geometry.x, geometry.y) = (x.round() as i32, y.round() as i32);
    }
}

unsafe extern "system" fn control_layer(
    _env: *mut JNIEnv,
    _class: jclass,
    _transaction: jlong,
    control: jlong,
    z: jint,
) {
    if let Some(layer) = SURFACE_CONTROLS.get(control) {
        layer.geometry().z = z;
    }
}

/// Only the order counts here: a layer placed below another sits below its own parent's pixels.
unsafe extern "system" fn control_relative_layer(
    _env: *mut JNIEnv,
    _class: jclass,
    _transaction: jlong,
    control: jlong,
    _relative_to: jlong,
    z: jint,
) {
    if let Some(layer) = SURFACE_CONTROLS.get(control) {
        layer.geometry().z = z;
    }
}

unsafe extern "system" fn control_flags(
    _env: *mut JNIEnv,
    _class: jclass,
    _transaction: jlong,
    control: jlong,
    flags: jint,
    mask: jint,
) {
    if mask & SURFACE_HIDDEN != 0
        && let Some(layer) = SURFACE_CONTROLS.get(control)
    {
        layer.geometry().hidden = flags & SURFACE_HIDDEN != 0;
    }
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
    /// Opens the window of a root layer. The compositor may choose another size, which the Bitmap
    /// follows. A child layer gets only the Bitmap, of the size its queue set.
    ///
    /// # Safety
    /// `env` must be the calling thread's JNI environment.
    unsafe fn open(env: *mut JNIEnv, layer: &Layer) -> Result<Target, Failure> {
        let (width, height) = layer.size();
        let window = if layer.parent().is_some() {
            if !(1..=MAX_CHILD_DIMENSION).contains(&width)
                || !(1..=MAX_CHILD_DIMENSION).contains(&height)
            {
                return Err(Failure::Message(format!("bad layer size {width}x{height}")));
            }
            None
        } else {
            Some(
                Window::open(&layer.name, width, height)
                    .map_err(|error| Failure::Message(format!("cannot open a window: {error}")))?,
            )
        };
        let (width, height) = window
            .as_ref()
            .map_or((width, height), |window| (window.width(), window.height()));
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
                queue: std::ptr::null_mut(),
                descriptor: std::ptr::null_mut(),
                watched: 0,
            })
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, `canvas` a Canvas, `dirty` a Rect or null.
unsafe fn lock(
    env: *mut JNIEnv,
    layer: &Arc<Layer>,
    canvas: jobject,
    dirty: jobject,
) -> Result<(), Failure> {
    let mut target = layer.target();
    if target.is_none() {
        // SAFETY: as above.
        let mut opened = unsafe { Target::open(env, layer)? };
        // SAFETY: as above.
        unsafe { watch(env, layer, &mut opened)? };
        *target = Some(opened);
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

/// Has the Looper of this thread watch a root layer's window, so its input and the compositor's
/// pings are read while nothing draws.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn watch(env: *mut JNIEnv, layer: &Arc<Layer>, target: &mut Target) -> Result<(), Failure> {
    let Some(window) = target.window.as_ref() else {
        return Ok(());
    };
    let class = WINDOW_INPUT_CLASS.load(Ordering::Acquire) as jclass;
    // SAFETY: as above; the class reference is global and the signatures match.
    unsafe {
        let looper = java::find_class(env, c"android/os/Looper")?;
        let queue =
            java::call_static_object(env, looper, c"myQueue", c"()Landroid/os/MessageQueue;", &[]);
        java::delete_local(env, looper);
        // A thread with no Looper draws, but nothing reads its window between frames.
        let queue = match queue {
            Ok(queue) if !queue.is_null() => queue,
            _ => {
                crate::clear_exception(env);
                return Ok(());
            }
        };
        let descriptor_class = java::find_class(env, c"java/io/FileDescriptor")?;
        let descriptor = java::new_object(env, descriptor_class, c"()V", &[])?;
        java::delete_local(env, descriptor_class);
        java::set_field(env, descriptor, c"descriptor", Field::Int(window.fd()))?;
        target.queue = java::global(env, queue);
        target.descriptor = java::global(env, descriptor);
        target.watched = WATCHED.add(Arc::downgrade(layer));
        let listener = java::new_object(env, class, c"(J)V", &[jvalue { j: target.watched }])?;
        let args = [
            jvalue {
                l: target.descriptor,
            },
            jvalue { i: EVENT_INPUT },
            jvalue { l: listener },
        ];
        let added = java::call_void(
            env,
            target.queue,
            c"addOnFileDescriptorEventListener",
            c"(Ljava/io/FileDescriptor;ILandroid/os/MessageQueue$OnFileDescriptorEventListener;)V",
            &args,
        );
        java::delete_local(env, listener);
        if added.is_err() {
            // Not watched: Drop must not remove a listener that was never added.
            java::delete_globals(&[target.queue]);
            target.queue = std::ptr::null_mut();
            added?;
        }
    }
    Ok(())
}

/// Sends a root window's new input down its layer's channel.
fn route(window: &mut Window, layer: &Layer) {
    let events = window.take_events();
    if events.is_empty() {
        return;
    }
    let input = layer.input.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(channel) = input.as_ref() {
        for event in &events {
            channel.send(event);
        }
    }
}

/// `WindowInput.dispatch(long)`: the window's connection is readable, on the Looper that drew it.
unsafe extern "system" fn window_dispatch(
    _env: *mut JNIEnv,
    _class: jclass,
    watched: jlong,
) -> jint {
    let Some(layer) = WATCHED.get(watched).and_then(|layer| layer.upgrade()) else {
        return 0;
    };
    let mut target = layer.target();
    let Some(window) = target.as_mut().and_then(|target| target.window.as_mut()) else {
        return 0;
    };
    if let Err(error) = window.dispatch() {
        eprintln!("uxdroid: the connection of {} failed: {error}", layer.name);
        return 0;
    }
    route(window, &layer);
    EVENT_INPUT
}

/// `WindowInput.attach(SurfaceControl, InputChannel)`: the window manager gives a window's layer the
/// server end of its input channel; null detaches it.
unsafe extern "system" fn window_attach(
    env: *mut JNIEnv,
    _class: jclass,
    control: jobject,
    channel: jobject,
) {
    if control.is_null() {
        return;
    }
    // SAFETY: ART passes a valid environment, a SurfaceControl and an InputChannel or null.
    let Ok(handle) = (unsafe { java::long_field(env, control, c"mNativeObject") }) else {
        return;
    };
    let Some(layer) = SURFACE_CONTROLS.get(handle) else {
        return;
    };
    // SAFETY: as above.
    let channel = unsafe { channel_of(env, channel) };
    *layer.input.lock().unwrap_or_else(|p| p.into_inner()) = channel;
}

fn placed(content: &Content, x: i32, y: i32) -> Placed<'_> {
    Placed {
        pixels: &content.pixels,
        width: content.width,
        height: content.height,
        x,
        y,
    }
}

/// Composes the root layer's window from its own last frame and its children's, and shows it.
fn present(target: &mut Target, root: &Layer) -> Result<(), Failure> {
    let Some(window) = target.window.as_mut() else {
        return Ok(());
    };
    let mut found = Vec::new();
    root.descendants(0, 0, false, &mut found);
    let contents: Vec<(MutexGuard<'_, Content>, i32, i32, bool)> = found
        .iter()
        .map(|(layer, x, y, below)| {
            (
                layer.content.lock().unwrap_or_else(|p| p.into_inner()),
                *x,
                *y,
                *below,
            )
        })
        .collect();
    let below: Vec<Placed<'_>> = contents
        .iter()
        .filter(|(_, _, _, below)| *below)
        .map(|(content, x, y, _)| placed(content, *x, *y))
        .collect();
    let above: Vec<Placed<'_>> = contents
        .iter()
        .filter(|(_, _, _, below)| !*below)
        .map(|(content, x, y, _)| placed(content, *x, *y))
        .collect();
    {
        let own = root.content.lock().unwrap_or_else(|p| p.into_inner());
        let frame = window.frame();
        let (width, height) = (frame.width, frame.height);
        compose(
            frame.pixels,
            width,
            height,
            &placed(&own, 0, 0),
            &below,
            &above,
        );
    }
    drop(contents);
    window
        .present()
        .map_err(|error| Failure::Message(format!("cannot present a frame: {error}")))?;
    // The round trip may have read input, which the Looper will not see as readable.
    route(window, root);
    Ok(())
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `canvas` the Canvas `lock` prepared.
unsafe fn post(env: *mut JNIEnv, layer: &Arc<Layer>, canvas: jobject) -> Result<(), Failure> {
    let mut guard = layer.target();
    let Some(target) = guard.as_mut() else {
        return Err(Failure::Message(format!("{} was never locked", layer.name)));
    };
    let (width, height) = (target.width, target.height);
    // SAFETY: as above. The content holds `width * height` pixels, the same as the array.
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
        let mut content = layer.content.lock().unwrap_or_else(|p| p.into_inner());
        content.pixels.resize((width * height) as usize, 0);
        (content.width, content.height) = (width, height);
        ((**env).v1_1.GetIntArrayRegion)(
            env,
            target.colors,
            0,
            width * height,
            content.pixels.as_mut_ptr().cast::<jint>(),
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
    if target.window.is_some() {
        present(target, layer)?;
    } else {
        // A child shows through its root's window, which may belong to another thread right now.
        drop(guard);
        let root = layer.root();
        if !Arc::ptr_eq(&root, layer) {
            let mut root_target = root.target();
            if let Some(root_target) = root_target.as_mut() {
                present(root_target, &root)?;
            }
        }
    }
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
    let control: [Method; 10] = [
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
        (c"nativeReparent", c"(JJJ)V", f!(control_reparent)),
        (c"nativeSetPosition", c"(JJFF)V", f!(control_position)),
        (c"nativeSetLayer", c"(JJI)V", f!(control_layer)),
        (
            c"nativeSetRelativeLayer",
            c"(JJJI)V",
            f!(control_relative_layer),
        ),
        (c"nativeSetFlags", c"(JJII)V", f!(control_flags)),
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
    let input: [Method; 2] = [
        (c"dispatch", c"(J)I", f!(window_dispatch)),
        (
            c"attach",
            c"(Landroid/view/SurfaceControl;Landroid/view/InputChannel;)V",
            f!(window_attach),
        ),
    ];
    // SAFETY: each table matches the framework's declarations in Android 16.
    unsafe {
        register(env, c"android/view/SurfaceControl", &control)?;
        register(env, c"android/graphics/BLASTBufferQueue", &queue)?;
        register(env, c"android/view/Surface", &surface)?;
        register(env, c"dev/uxdroid/system/WindowInput", &input)?;
        let class = java::find_class(env, c"dev/uxdroid/system/WindowInput")
            .map_err(|Pending| "class dev/uxdroid/system/WindowInput not found".to_string())?;
        WINDOW_INPUT_CLASS.store(java::global(env, class).cast(), Ordering::Release);
    }
    Ok(())
}
