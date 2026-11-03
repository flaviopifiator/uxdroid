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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock, Weak};
use std::time::Instant;

use jni_sys::{JNIEnv, jboolean, jclass, jint, jintArray, jlong, jobject, jstring, jvalue};

use crate::compose::Rect;
use crate::handles::Handles;
use crate::input::natives::{Channel, channel_of};
use crate::java::{self, Field, Pending};
use crate::scene::{self, Change, Composer, Transaction};
use crate::wayland::{self, Window};
use crate::windows::{Placement, Source, Stack};
use crate::{Method, java_string, register, throw};

/// Bounds a child layer's Bitmap, which no compositor sizes.
const MAX_CHILD_DIMENSION: i32 = 16384;

/// When this library loaded, for the first-frame report.
pub static LOADED: OnceLock<Instant> = OnceLock::new();

/// What a layer keeps beside its place in the tree: the size the framework asked for, where it
/// draws once it first draws, and for a window's own layer, its input.
#[derive(Default)]
pub struct Platform {
    size: Mutex<(i32, i32)>,
    target: Mutex<Option<Target>>,
    posted: AtomicBool,
    /// The server end of the window's input channel, for a window's own layer.
    input: Mutex<Option<Channel>>,
    /// Transactions merged into the next frame of this layer's buffer queue.
    pending: Mutex<Transaction<Platform>>,
    /// Where the window manager put a window's own layer on the display, and its flags.
    placement: Mutex<Option<Placement>>,
    /// The window manager removed this layer's window.
    closed: AtomicBool,
}

type Layer = scene::Layer<Platform>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl scene::Layer<Platform> {
    fn size(&self) -> (i32, i32) {
        *locked(&self.extra.size)
    }

    fn set_size(&self, width: i32, height: i32) {
        *locked(&self.extra.size) = (width, height);
    }

    fn target(&self) -> MutexGuard<'_, Option<Target>> {
        locked(&self.extra.target)
    }

    /// Whether this layer's frames go to a window of its own.
    fn owns_window(self: &Arc<Self>) -> bool {
        self.window_layer()
            .is_some_and(|window| Arc::ptr_eq(&window, self))
    }
}

/// Where a layer draws: the Wayland window of a window's own layer, or nothing for a child, and the
/// Bitmap the framework draws it into.
struct Target {
    window: Option<Window>,
    composer: Composer,
    bitmap: jobject,
    colors: jintArray,
    width: i32,
    height: i32,
    /// The part of the Bitmap the framework redraws for the frame being drawn.
    dirty: Rect,
    /// Whether the next frame must be drawn whole, as the first one is.
    fresh: bool,
}

// SAFETY: the window's mapping is plain process memory that only this Target uses, its Wayland
// proxies may be used from any thread, and both Java references are global. Every use goes through
// the Layer's mutex, so only one thread touches a Target at a time.
unsafe impl Send for Target {}

impl Drop for Target {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            let changes = locked(&STACK).remove(window.key());
            locked(&WINDOWS).retain(|(key, _)| *key != window.key());
            send(changes);
        }
        // SAFETY: these are this Target's own global references.
        unsafe { java::delete_globals(&[self.bitmap, self.colors]) };
    }
}

static SURFACE_CONTROLS: Handles<Arc<Layer>> = Handles::new();
static SURFACES: Handles<Arc<Layer>> = Handles::new();
static QUEUES: Handles<Arc<Mutex<Option<Arc<Layer>>>>> = Handles::new();
/// The display's windows, for input: where each is, and which layer each surface shows.
static STACK: Mutex<Stack<u32>> = Mutex::new(Stack::new());
static WINDOWS: Mutex<Vec<(u32, Weak<Layer>)>> = Mutex::new(Vec::new());
/// Whether a Looper watches the display's connection.
static WATCHING: AtomicBool = AtomicBool::new(false);
/// Frames the windows presented, which an agent's `wait` for idle watches.
static PRESENTED: AtomicU64 = AtomicU64::new(0);
/// `dev.uxdroid.system.WindowService`, for what the compositor asks of the display.
static WINDOW_SERVICE_CLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
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
    let parent = (parent != 0)
        .then(|| SURFACE_CONTROLS.get(parent))
        .flatten();
    let layer = Layer::new(
        name,
        parent,
        flags & CREATE_HIDDEN != 0,
        Platform::default(),
    );
    layer.set_size(width, height);
    SURFACE_CONTROLS.add(layer)
}

unsafe extern "system" fn control_copy(_env: *mut JNIEnv, _class: jclass, control: jlong) -> jlong {
    SURFACE_CONTROLS
        .get(control)
        .map_or(0, |layer| SURFACE_CONTROLS.add(layer))
}

/// The `hidden` flag of `SurfaceControl.Builder`.
const CREATE_HIDDEN: jint = 0x4;

/// Transactions, by the handle `SurfaceControl.Transaction` keeps.
static TRANSACTIONS: Handles<Arc<Mutex<Transaction<Platform>>>> = Handles::new();

unsafe extern "system" fn transaction_create(_env: *mut JNIEnv, _class: jclass) -> jlong {
    TRANSACTIONS.add(Arc::default())
}

/// What `NativeAllocationRegistry` calls, on any thread, when a Transaction is released.
extern "C" fn transaction_free(transaction: *mut c_void) {
    drop(TRANSACTIONS.remove(transaction as i64));
}

unsafe extern "system" fn transaction_finalizer(_env: *mut JNIEnv, _class: jclass) -> jlong {
    transaction_free as *const () as jlong
}

/// Shows again every window whose layers a transaction changed.
fn apply(transaction: &mut Transaction<Platform>) {
    for window in transaction.apply() {
        if let Err(Failure::Message(message)) = present_window(&window) {
            uxdroid_bridge::report!("uxdroid: {message}");
        }
    }
}

unsafe extern "system" fn transaction_apply(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    _sync: jboolean,
    _one_way: jboolean,
) {
    if let Some(transaction) = TRANSACTIONS.get(transaction) {
        // Taken out first, so presenting runs with no transaction locked.
        let mut changes = std::mem::take(&mut *locked(&transaction));
        apply(&mut changes);
    }
}

unsafe extern "system" fn transaction_merge(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    other: jlong,
) {
    let (Some(into), Some(from)) = (TRANSACTIONS.get(transaction), TRANSACTIONS.get(other)) else {
        return;
    };
    if Arc::ptr_eq(&into, &from) {
        return;
    }
    let mut taken = std::mem::take(&mut *locked(&from));
    locked(&into).merge(&mut taken);
}

unsafe extern "system" fn transaction_clear(_env: *mut JNIEnv, _class: jclass, transaction: jlong) {
    if let Some(transaction) = TRANSACTIONS.get(transaction) {
        locked(&transaction).clear();
    }
}

/// Adds a change of `control`'s layer to `transaction`. A handle that is no transaction of ours,
/// such as one a default body made, applies the change at once, as every change did before.
fn set(transaction: jlong, control: jlong, change: impl FnOnce() -> Option<Change<Platform>>) {
    let Some(layer) = SURFACE_CONTROLS.get(control) else {
        return;
    };
    let Some(change) = change() else {
        return;
    };
    match TRANSACTIONS.get(transaction) {
        Some(transaction) => locked(&transaction).set(layer, change),
        None => {
            static UNKNOWN: Once = Once::new();
            UNKNOWN.call_once(|| {
                uxdroid_bridge::report!("uxdroid: a SurfaceControl change came with no transaction of ours; applied at once")
            });
            let mut once = Transaction::default();
            once.set(layer, change);
            apply(&mut once);
        }
    }
}

unsafe extern "system" fn control_reparent(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    parent: jlong,
) {
    set(transaction, control, || {
        Some(Change::Reparent(SURFACE_CONTROLS.get(parent)))
    });
}

unsafe extern "system" fn control_position(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    x: f32,
    y: f32,
) {
    set(transaction, control, || {
        (x.is_finite() && y.is_finite()).then_some(Change::Position(x, y))
    });
}

unsafe extern "system" fn control_layer(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    z: jint,
) {
    set(transaction, control, || Some(Change::Layer(z)));
}

unsafe extern "system" fn control_relative_layer(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    relative_to: jlong,
    z: jint,
) {
    set(transaction, control, || {
        Some(match SURFACE_CONTROLS.get(relative_to) {
            Some(relative) => Change::RelativeLayer(relative, z),
            None => Change::Layer(z),
        })
    });
}

unsafe extern "system" fn control_flags(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    flags: jint,
    mask: jint,
) {
    set(transaction, control, || {
        Some(Change::Flags {
            flags: flags as u32,
            mask: mask as u32,
        })
    });
}

unsafe extern "system" fn control_alpha(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    alpha: f32,
) {
    set(transaction, control, || {
        alpha.is_finite().then_some(Change::Alpha(alpha))
    });
}

fn finite(values: [f32; 4]) -> Option<[f32; 4]> {
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

unsafe extern "system" fn control_matrix(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    dsdx: f32,
    dtdx: f32,
    dtdy: f32,
    dsdy: f32,
) {
    set(transaction, control, || {
        finite([dsdx, dtdx, dtdy, dsdy]).map(Change::Matrix)
    });
}

unsafe extern "system" fn control_scale(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    x: f32,
    y: f32,
) {
    set(transaction, control, || {
        finite([x, 0.0, 0.0, y]).map(Change::Matrix)
    });
}

unsafe extern "system" fn control_crop(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) {
    set(transaction, control, || {
        Some(Change::Crop(finite([left, top, right, bottom])))
    });
}

unsafe extern "system" fn control_window_crop(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    left: jint,
    top: jint,
    right: jint,
    bottom: jint,
) {
    set(transaction, control, || {
        Some(Change::Crop(Some(
            [left, top, right, bottom].map(|side| side as f32),
        )))
    });
}

unsafe extern "system" fn control_corner_radius(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    radius: f32,
) {
    set(transaction, control, || {
        radius.is_finite().then_some(Change::CornerRadius(radius))
    });
}

unsafe extern "system" fn control_buffer_transform(
    _env: *mut JNIEnv,
    _class: jclass,
    transaction: jlong,
    control: jlong,
    transform: jint,
) {
    set(transaction, control, || {
        Some(Change::BufferTransform(transform as u32))
    });
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

/// `mergeWithNextTransaction`: the changes wait for the queue's next frame, as SurfaceView's
/// position does for its window's. A queue with no layer yet applies them at once.
unsafe extern "system" fn queue_merge_with_next(
    _env: *mut JNIEnv,
    _class: jclass,
    queue: jlong,
    transaction: jlong,
    _frame: jlong,
) {
    let Some(transaction) = TRANSACTIONS.get(transaction) else {
        return;
    };
    let mut taken = std::mem::take(&mut *locked(&transaction));
    match queue_layer(queue) {
        Some(layer) => locked(&layer.extra.pending).merge(&mut taken),
        None => apply(&mut taken),
    }
}

unsafe extern "system" fn queue_apply_pending(
    _env: *mut JNIEnv,
    _class: jclass,
    queue: jlong,
    _frame: jlong,
) {
    if let Some(layer) = queue_layer(queue) {
        let mut pending = std::mem::take(&mut *locked(&layer.extra.pending));
        apply(&mut pending);
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

/// A Bitmap and the array its pixels cross into Rust through, both global references.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn bitmap(
    env: *mut JNIEnv,
    width: i32,
    height: i32,
) -> Result<(jobject, jintArray), Failure> {
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
        Ok((java::global(env, bitmap), java::global(env, colors)))
    }
}

fn checked(width: i32, height: i32) -> Result<(), Failure> {
    if !(1..=MAX_CHILD_DIMENSION).contains(&width) || !(1..=MAX_CHILD_DIMENSION).contains(&height) {
        return Err(Failure::Message(format!("bad layer size {width}x{height}")));
    }
    Ok(())
}

impl Target {
    /// Opens the window of a window's own layer: the display's toplevel, whose size the compositor
    /// may choose and the Bitmap follows, or a subsurface where the window manager placed it. Any
    /// other layer gets only the Bitmap, of the size its queue set.
    ///
    /// # Safety
    /// `env` must be the calling thread's JNI environment.
    unsafe fn open(env: *mut JNIEnv, layer: &Arc<Layer>) -> Result<Target, Failure> {
        let (width, height) = layer.size();
        let placement = *locked(&layer.extra.placement);
        let window = if layer.owns_window() {
            if layer.extra.closed.load(Ordering::Acquire) {
                return Err(Failure::Message(format!("{} was removed", layer.name)));
            }
            let (x, y) = placement.map_or((0, 0), |placement| placement.surface());
            let window = Window::open(&layer.name, width, height, x, y)
                .map_err(|error| Failure::Message(format!("cannot open a window: {error}")))?;
            let key = window.key();
            locked(&WINDOWS).push((key, Arc::downgrade(layer)));
            let placement = placement.unwrap_or(Placement {
                x: 0,
                y: 0,
                width: window.width(),
                height: window.height(),
                flags: 0,
                inset_left: 0,
                inset_top: 0,
            });
            let changes = locked(&STACK).place(key, placement);
            send(changes);
            Some(window)
        } else {
            checked(width, height)?;
            None
        };
        let (width, height) = window
            .as_ref()
            .map_or((width, height), |window| (window.width(), window.height()));
        // SAFETY: as above.
        let (bitmap, colors) = unsafe { bitmap(env, width, height)? };
        Ok(Target {
            window,
            composer: Composer::default(),
            bitmap,
            colors,
            width,
            height,
            dirty: Rect::sized(width, height),
            fresh: true,
        })
    }

    /// Follows a new size the framework asked for, such as after the display was resized: a new
    /// Bitmap and window buffer, and a whole frame next.
    ///
    /// # Safety
    /// `env` must be the calling thread's JNI environment.
    unsafe fn reshape(&mut self, env: *mut JNIEnv, width: i32, height: i32) -> Result<(), Failure> {
        checked(width, height)?;
        if let Some(window) = self.window.as_mut() {
            window
                .resize(width, height)
                .map_err(|error| Failure::Message(format!("cannot resize a window: {error}")))?;
        }
        // SAFETY: as above.
        let (bitmap, colors) = unsafe { bitmap(env, width, height)? };
        // SAFETY: the old references are this Target's own.
        unsafe { java::delete_globals(&[self.bitmap, self.colors]) };
        (self.bitmap, self.colors) = (bitmap, colors);
        (self.width, self.height) = (width, height);
        self.fresh = true;
        Ok(())
    }
}

/// The part of a frame the framework asks to redraw: `dirty` within the Bitmap, or all of it when
/// `dirty` is null or empty, or the Bitmap has no frame yet.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `dirty` a Rect or null.
unsafe fn requested(env: *mut JNIEnv, dirty: jobject, target: &Target) -> Result<Rect, Pending> {
    let all = Rect::sized(target.width, target.height);
    if dirty.is_null() || target.fresh {
        return Ok(all);
    }
    // SAFETY: as above.
    let rect = unsafe {
        Rect::new(
            java::int_field(env, dirty, c"left")?,
            java::int_field(env, dirty, c"top")?,
            java::int_field(env, dirty, c"right")?,
            java::int_field(env, dirty, c"bottom")?,
        )
    };
    let rect = rect.intersect(&all);
    Ok(if rect.is_empty() { all } else { rect })
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
        let opened = unsafe { Target::open(env, layer)? };
        if opened.window.is_some() {
            // SAFETY: as above.
            unsafe { watch(env)? };
        }
        *target = Some(opened);
    }
    let Some(target) = target.as_mut() else {
        return Err(Failure::Message("no window".into()));
    };
    let (width, height) = layer.size();
    if (width, height) != (target.width, target.height) && width > 0 && height > 0 {
        // SAFETY: as above.
        unsafe { target.reshape(env, width, height)? };
    }
    // SAFETY: as above; the Bitmap is mutable and lives as long as the Target.
    unsafe {
        // The Bitmap keeps the last frame, so only the part asked for is redrawn, as Android's
        // Surface does with its back buffer: the Canvas is clipped to it.
        target.dirty = requested(env, dirty, target)?;
        java::call_void(
            env,
            canvas,
            c"setBitmap",
            c"(Landroid/graphics/Bitmap;)V",
            &[jvalue { l: target.bitmap }],
        )?;
        let Rect {
            left,
            top,
            right,
            bottom,
        } = target.dirty;
        let sides = [left, top, right, bottom].map(|i| jvalue { i });
        if !dirty.is_null() {
            java::call_void(env, dirty, c"set", c"(IIII)V", &sides)?;
        }
        let id = java::method(env, canvas, c"clipRect", c"(IIII)Z")?;
        ((**env).v1_1.CallBooleanMethodA)(env, canvas, id, sides.as_ptr());
        java::check(env)?;
    }
    Ok(())
}

/// Has the Looper of this thread watch the display's connection, once for the process, so input
/// and the compositor's pings are read while nothing draws.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn watch(env: *mut JNIEnv) -> Result<(), Failure> {
    let Some(fd) = wayland::fd() else {
        return Ok(());
    };
    if WATCHING.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let class = WINDOW_INPUT_CLASS.load(Ordering::Acquire) as jclass;
    // SAFETY: as above; the class reference is global and the signatures match.
    let watched = unsafe {
        (|| -> Result<bool, Pending> {
            let looper = java::find_class(env, c"android/os/Looper")?;
            let queue = java::call_static_object(
                env,
                looper,
                c"myQueue",
                c"()Landroid/os/MessageQueue;",
                &[],
            );
            java::delete_local(env, looper);
            // A thread with no Looper draws, but nothing reads the display between frames.
            let queue = match queue {
                Ok(queue) if !queue.is_null() => queue,
                _ => {
                    crate::clear_exception(env);
                    return Ok(false);
                }
            };
            let descriptor_class = java::find_class(env, c"java/io/FileDescriptor")?;
            let descriptor = java::new_object(env, descriptor_class, c"()V", &[])?;
            java::delete_local(env, descriptor_class);
            java::set_field(env, descriptor, c"descriptor", Field::Int(fd))?;
            let listener = java::new_object(env, class, c"(J)V", &[jvalue { j: 0 }])?;
            let args = [
                jvalue { l: descriptor },
                jvalue { i: EVENT_INPUT },
                jvalue { l: listener },
            ];
            java::call_void(
                env,
                queue,
                c"addOnFileDescriptorEventListener",
                c"(Ljava/io/FileDescriptor;ILandroid/os/MessageQueue$OnFileDescriptorEventListener;)V",
                &args,
            )?;
            java::delete_local(env, listener);
            Ok(true)
        })()
    };
    match watched {
        Ok(true) => Ok(()),
        // Another thread with a Looper may watch it later.
        Ok(false) => {
            WATCHING.store(false, Ordering::Release);
            Ok(())
        }
        Err(Pending) => {
            WATCHING.store(false, Ordering::Release);
            Err(Failure::Pending)
        }
    }
}

/// Sends events to the channels of the windows they are for.
fn send(events: Vec<(u32, crate::input::Event)>) {
    for (key, event) in events {
        let layer = locked(&WINDOWS)
            .iter()
            .find(|(window, _)| *window == key)
            .and_then(|(_, layer)| layer.upgrade());
        if let Some(layer) = layer
            && let Some(channel) = locked(&layer.extra.input).as_ref()
        {
            channel.send(&event);
        }
    }
}

/// Gives the display's new input to the windows Android would give it to.
fn route() {
    let events = wayland::take_events();
    if events.is_empty() {
        return;
    }
    let routed: Vec<(u32, crate::input::Event)> = {
        let mut stack = locked(&STACK);
        events
            .into_iter()
            .flat_map(|(surface, event)| {
                let source = surface.map_or(Source::Focused, Source::Surface);
                stack.route(source, event)
            })
            .collect()
    };
    send(routed);
}

/// Hands what the compositor asked of the display to the window manager, on a Looper thread.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn act(env: *mut JNIEnv) {
    let requests = wayland::take_requests();
    let class = WINDOW_SERVICE_CLASS.load(Ordering::Acquire) as jclass;
    if class.is_null() {
        return;
    }
    // SAFETY: as above; the class reference is global and both methods are static.
    unsafe {
        if let Some((width, height)) = requests.resize {
            let args = [jvalue { i: width }, jvalue { i: height }];
            if java::call_static_void(env, class, c"displayResized", c"(II)V", &args).is_err() {
                crate::clear_exception(env);
                uxdroid_bridge::report!("uxdroid: the window manager failed to resize the display");
            }
        }
        if requests.close
            && java::call_static_void(env, class, c"displayClosed", c"()V", &[]).is_err()
        {
            crate::clear_exception(env);
            uxdroid_bridge::report!("uxdroid: the window manager failed to close the display");
        }
    }
}

/// `WindowInput.dispatch(long)`: the display's connection is readable, on the Looper watching it.
unsafe extern "system" fn window_dispatch(
    env: *mut JNIEnv,
    _class: jclass,
    _watched: jlong,
) -> jint {
    if let Err(error) = wayland::dispatch() {
        uxdroid_bridge::report!("uxdroid: the connection to the compositor failed: {error}");
        return 0;
    }
    route();
    // SAFETY: ART passes a valid environment.
    unsafe { act(env) };
    EVENT_INPUT
}

/// The layer of a `SurfaceControl` object.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `control` a SurfaceControl or null.
unsafe fn layer_of(env: *mut JNIEnv, control: jobject) -> Option<Arc<Layer>> {
    if control.is_null() {
        return None;
    }
    // SAFETY: as above.
    let handle = unsafe { java::long_field(env, control, c"mNativeObject") }.ok()?;
    SURFACE_CONTROLS.get(handle)
}

/// `WindowInput.attach(SurfaceControl, InputChannel)`: the window manager gives a window's layer the
/// server end of its input channel; null detaches it.
unsafe extern "system" fn window_attach(
    env: *mut JNIEnv,
    _class: jclass,
    control: jobject,
    channel: jobject,
) {
    // SAFETY: ART passes a valid environment, a SurfaceControl and an InputChannel or null.
    let Some(layer) = (unsafe { layer_of(env, control) }) else {
        return;
    };
    // SAFETY: as above.
    let channel = unsafe { channel_of(env, channel) };
    *locked(&layer.extra.input) = channel;
}

/// `WindowInput.close(SurfaceControl)`: the window manager removed the window, whose surface leaves
/// the display at once.
unsafe extern "system" fn window_close(env: *mut JNIEnv, _class: jclass, control: jobject) {
    // SAFETY: ART passes a valid environment and a SurfaceControl or null.
    let Some(layer) = (unsafe { layer_of(env, control) }) else {
        return;
    };
    layer.extra.closed.store(true, Ordering::Release);
    let target = layer.target().take();
    drop(target);
}

/// `WindowInput.place(SurfaceControl, int, int, int, int, int, int, int)`: where the window manager
/// put a window's frame on the display, its `LayoutParams` flags, and how far its surface starts
/// before the frame.
unsafe extern "system" fn window_place(
    env: *mut JNIEnv,
    _class: jclass,
    control: jobject,
    x: jint,
    y: jint,
    width: jint,
    height: jint,
    flags: jint,
    inset_left: jint,
    inset_top: jint,
) {
    // SAFETY: ART passes a valid environment and a SurfaceControl or null.
    let Some(layer) = (unsafe { layer_of(env, control) }) else {
        return;
    };
    let placement = Placement {
        x,
        y,
        width,
        height,
        flags,
        inset_left,
        inset_top,
    };
    *locked(&layer.extra.placement) = Some(placement);
    let key = {
        let target = layer.target();
        let Some(window) = target.as_ref().and_then(|target| target.window.as_ref()) else {
            return;
        };
        let (x, y) = placement.surface();
        if let Err(error) = window.place(x, y) {
            uxdroid_bridge::report!("uxdroid: cannot move {}: {error}", layer.name);
        }
        window.key()
    };
    let changes = locked(&STACK).place(key, placement);
    send(changes);
}

/// Composes what changed in a window from its layers' last frames, and shows it.
fn present(target: &mut Target, window_layer: &Arc<Layer>) -> Result<(), Failure> {
    let Target {
        window, composer, ..
    } = target;
    let Some(window) = window.as_mut() else {
        return Ok(());
    };
    let frame = window.frame();
    let (width, height) = (frame.width, frame.height);
    let damage = composer.compose(window_layer, frame.pixels, width, height);
    if damage.is_empty() {
        return Ok(());
    }
    window
        .present(damage)
        .map_err(|error| Failure::Message(format!("cannot present a frame: {error}")))?;
    PRESENTED.fetch_add(1, Ordering::Relaxed);
    // The round trip may have read input, which the Looper will not see as readable.
    route();
    Ok(())
}

/// Shows again a window whose layers changed, from whichever thread changed them.
fn present_window(window_layer: &Arc<Layer>) -> Result<(), Failure> {
    let mut target = window_layer.target();
    match target.as_mut() {
        Some(target) => present(target, window_layer),
        None => Ok(()),
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `canvas` the Canvas `lock` prepared.
unsafe fn post(env: *mut JNIEnv, layer: &Arc<Layer>, canvas: jobject) -> Result<(), Failure> {
    let mut guard = layer.target();
    let Some(target) = guard.as_mut() else {
        return Err(Failure::Message(format!("{} was never locked", layer.name)));
    };
    let (width, height) = (target.width, target.height);
    let dirty = target.dirty;
    // SAFETY: as above. The array and the content hold `width * height` pixels, row after row, and
    // both keep the last frame, so only the dirty rows and columns are copied into each.
    unsafe {
        let start = dirty.top * width + dirty.left;
        let args = [
            jvalue { l: target.colors },
            jvalue { i: start },
            jvalue { i: width },
            jvalue { i: dirty.left },
            jvalue { i: dirty.top },
            jvalue { i: dirty.width() },
            jvalue { i: dirty.height() },
        ];
        java::call_void(env, target.bitmap, c"getPixels", c"([IIIIIII)V", &args)?;
        let mut content = layer.content();
        if (content.width, content.height) != (width, height) {
            content.pixels = vec![0; (width * height) as usize];
            (content.width, content.height) = (width, height);
        }
        let end = (dirty.bottom - 1) * width + dirty.right;
        ((**env).v1_1.GetIntArrayRegion)(
            env,
            target.colors,
            start,
            end - start,
            content
                .pixels
                .as_mut_ptr()
                .add(start as usize)
                .cast::<jint>(),
        );
        content.damage = content.damage.union(&dirty);
        drop(content);
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
    target.fresh = false;
    // Changes merged into this frame show with it.
    let mut pending = std::mem::take(&mut *locked(&layer.extra.pending));
    let windows = pending.apply();
    if target.window.is_some() {
        present(target, layer)?;
    }
    drop(guard);
    // A child shows through its window, which may belong to another thread right now.
    let mut others: Vec<Arc<Layer>> = windows
        .into_iter()
        .filter(|window| !Arc::ptr_eq(window, layer))
        .collect();
    if let Some(window) = layer.window_layer()
        && !Arc::ptr_eq(&window, layer)
        && !others.iter().any(|other| Arc::ptr_eq(other, &window))
    {
        others.push(window);
    }
    for window in others {
        present_window(&window)?;
    }
    if !layer.extra.posted.swap(true, Ordering::Relaxed) {
        let since = LOADED.get().map_or(0, |start| start.elapsed().as_millis());
        uxdroid_bridge::report!(
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
    let control: [Method; 22] = [
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
        (c"nativeCreateTransaction", c"()J", f!(transaction_create)),
        (
            c"nativeGetNativeTransactionFinalizer",
            c"()J",
            f!(transaction_finalizer),
        ),
        (c"nativeApplyTransaction", c"(JZZ)V", f!(transaction_apply)),
        (c"nativeMergeTransaction", c"(JJ)V", f!(transaction_merge)),
        (c"nativeClearTransaction", c"(J)V", f!(transaction_clear)),
        (c"nativeReparent", c"(JJJ)V", f!(control_reparent)),
        (c"nativeSetPosition", c"(JJFF)V", f!(control_position)),
        (c"nativeSetLayer", c"(JJI)V", f!(control_layer)),
        (
            c"nativeSetRelativeLayer",
            c"(JJJI)V",
            f!(control_relative_layer),
        ),
        (c"nativeSetFlags", c"(JJII)V", f!(control_flags)),
        (c"nativeSetAlpha", c"(JJF)V", f!(control_alpha)),
        (c"nativeSetMatrix", c"(JJFFFF)V", f!(control_matrix)),
        (c"nativeSetScale", c"(JJFF)V", f!(control_scale)),
        (c"nativeSetCrop", c"(JJFFFF)V", f!(control_crop)),
        (
            c"nativeSetWindowCrop",
            c"(JJIIII)V",
            f!(control_window_crop),
        ),
        (
            c"nativeSetCornerRadius",
            c"(JJF)V",
            f!(control_corner_radius),
        ),
        (
            c"nativeSetBufferTransform",
            c"(JJI)V",
            f!(control_buffer_transform),
        ),
    ];
    let queue: [Method; 7] = [
        (
            c"nativeMergeWithNextTransaction",
            c"(JJJ)V",
            f!(queue_merge_with_next),
        ),
        (
            c"nativeApplyPendingTransactions",
            c"(JJ)V",
            f!(queue_apply_pending),
        ),
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
    let input: [Method; 4] = [
        (
            c"close",
            c"(Landroid/view/SurfaceControl;)V",
            f!(window_close),
        ),
        (c"dispatch", c"(J)I", f!(window_dispatch)),
        (
            c"attach",
            c"(Landroid/view/SurfaceControl;Landroid/view/InputChannel;)V",
            f!(window_attach),
        ),
        (
            c"place",
            c"(Landroid/view/SurfaceControl;IIIIIII)V",
            f!(window_place),
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
        let class = java::find_class(env, c"dev/uxdroid/system/WindowService")
            .map_err(|Pending| "class dev/uxdroid/system/WindowService not found".to_string())?;
        WINDOW_SERVICE_CLASS.store(java::global(env, class).cast(), Ordering::Release);
    }
    Ok(())
}

/// The display as an agent sees it: its seat, and its windows' last frames.
pub struct DisplaySeat;

impl crate::control::agent::Seat for DisplaySeat {
    fn display(&self) -> Option<(i32, i32)> {
        wayland::display_size()
    }

    /// Display coordinates become the bottom window's, which input from the compositor's seat
    /// would be in, and routing finds the window under them as it does for that input.
    fn send(&self, input: crate::control::agent::Input) -> Result<(), String> {
        use crate::control::agent::Input;
        use wayland::Injected;
        let (surface, placement) = locked(&STACK)
            .windows()
            .first()
            .copied()
            .ok_or("the app shows no window")?;
        // Routing adds the window's place back, so the event lands at the display point.
        let (dx, dy) = placement.surface();
        let at = |x: i32, y: i32| ((x - dx) as f32, (y - dy) as f32);
        let injected = match input {
            Input::TouchDown { x, y } => {
                let (x, y) = at(x, y);
                Injected::TouchDown { surface, x, y }
            }
            Input::TouchUp => Injected::TouchUp { surface },
            Input::Wheel { x, y, dx, dy } => {
                let (x, y) = at(x, y);
                Injected::Wheel {
                    surface,
                    x,
                    y,
                    vertical: dy as f32,
                    horizontal: dx as f32,
                }
            }
            Input::Key {
                evdev,
                pressed,
                shift,
            } => Injected::Key {
                evdev,
                pressed,
                shift,
            },
        };
        wayland::inject(injected)?;
        route();
        Ok(())
    }

    fn presented(&self) -> u64 {
        PRESENTED.load(Ordering::Relaxed)
    }

    /// Copies each window's frame under its own lock, one at a time: drawing holds a window's lock
    /// while it takes the stack's, so the stack is never held while a window is locked here.
    fn frames(&self) -> Vec<crate::control::agent::WindowFrame> {
        let stack = locked(&STACK).windows();
        let windows: Vec<(u32, Arc<Layer>)> = locked(&WINDOWS)
            .iter()
            .filter_map(|(key, layer)| Some((*key, layer.upgrade()?)))
            .collect();
        let mut frames = Vec::new();
        for (key, placement) in stack {
            let Some((_, layer)) = windows.iter().find(|(window, _)| *window == key) else {
                continue;
            };
            let mut target = layer.target();
            let Some(window) = target.as_mut().and_then(|target| target.window.as_mut()) else {
                continue;
            };
            let (x, y) = if window.is_toplevel() {
                (0, 0)
            } else {
                placement.surface()
            };
            let frame = window.frame();
            let count = (frame.width * frame.height) as usize;
            frames.push(crate::control::agent::WindowFrame {
                x,
                y,
                width: frame.width,
                height: frame.height,
                pixels: frame.pixels[..count.min(frame.pixels.len())].to_vec(),
            });
        }
        frames
    }

    fn charmap(&self) -> Arc<crate::input::charmap::Charmap> {
        crate::input::charmap::seat()
    }
}
