//! `ANativeWindow` over the framework's `android.view.Surface`.
//!
//! A native window is the Java Surface the app passed. Locking hands out a buffer of this module,
//! and posting copies it into a Bitmap with `setPixels` and draws that through the Surface's own
//! `lockCanvas` and `unlockCanvasAndPost`, the path `ViewRootImpl` uses, which `libuxdroid_system.so`
//! shows on Wayland. Going through Java keeps this library apart from the window system's internals,
//! at the cost of three copies per frame; a buffer shared with the Wayland window would remove them.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jni_sys::{JNIEnv, jobject, jvalue};

use crate::bitmap::{FORMAT_RGB_565, FORMAT_RGBA_8888, Layout, new_bitmap, transfer};
use crate::java::{Jni, Thrown};
use crate::live::Live;

const FORMAT_RGBX_8888: i32 = 2;

/// `ANativeWindow_Buffer` from `<android/native_window.h>`.
#[repr(C)]
pub struct Buffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
}

/// `ARect` from `<android/rect.h>`.
#[repr(C)]
pub struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Clone, Copy, Default)]
struct Geometry {
    width: i32,
    height: i32,
    format: i32,
}

/// What a lock handed out, until it is posted.
struct Frame {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    format: i32,
}

struct Drawing {
    locked: Option<Frame>,
    spare: Vec<u8>,
    /// A global reference to the Bitmap posting copies into, with its size and format.
    bitmap: Option<(jobject, i32, i32, i32)>,
}

pub struct Window {
    surface: jobject,
    references: AtomicUsize,
    geometry: Mutex<Geometry>,
    drawing: Mutex<Drawing>,
}

// SAFETY: the Java references are global, and the rest is behind mutexes or atomic.
unsafe impl Send for Window {}
// SAFETY: as above.
unsafe impl Sync for Window {}

static WINDOWS: Live<Window> = Live::new();

pub fn get(window: *const c_void) -> Option<Arc<Window>> {
    WINDOWS.get(window)
}

fn bytes_per_pixel(format: i32) -> usize {
    if format == FORMAT_RGB_565 { 2 } else { 4 }
}

impl Window {
    /// The Surface's size, which the window system gives the layer.
    fn surface_size(&self, jni: Jni) -> Result<(i32, i32), Thrown> {
        // SAFETY: a live Surface; its private natives are the framework's own, which JNI may call.
        unsafe {
            let handle = jni.field(self.surface, c"mNativeObject", c"J")?;
            let class = jni.find_class(c"android/view/Surface")?;
            let width = jni.call_static(class, c"nativeGetWidth", c"(J)I", &[jvalue { j: handle }]);
            let height =
                jni.call_static(class, c"nativeGetHeight", c"(J)I", &[jvalue { j: handle }]);
            jni.delete_local(class);
            Ok((width?.i, height?.i))
        }
    }

    fn geometry(&self) -> Geometry {
        *self.geometry.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The buffer size and format a lock gets now.
    pub fn buffer_size(&self, jni: Jni) -> Result<(i32, i32, i32), Thrown> {
        let geometry = self.geometry();
        let (width, height) = if geometry.width > 0 && geometry.height > 0 {
            (geometry.width, geometry.height)
        } else {
            self.surface_size(jni)?
        };
        let format = if geometry.format == 0 {
            FORMAT_RGBA_8888
        } else {
            geometry.format
        };
        Ok((width, height, format))
    }

    fn drawing(&self) -> std::sync::MutexGuard<'_, Drawing> {
        self.drawing.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Hands out a buffer of the window's size and format.
    pub fn lock(&self, jni: Jni) -> Result<(*mut u8, i32, i32, i32), Thrown> {
        let (width, height, format) = self.buffer_size(jni)?;
        self.lock_frame(width, height, format)
    }

    /// Hands out an RGBA buffer of this size, which posting scales to the window.
    pub fn lock_exact(
        &self,
        _jni: Jni,
        width: i32,
        height: i32,
    ) -> Result<(*mut u8, i32, i32, i32), Thrown> {
        self.lock_frame(width, height, FORMAT_RGBA_8888)
    }

    fn lock_frame(
        &self,
        width: i32,
        height: i32,
        format: i32,
    ) -> Result<(*mut u8, i32, i32, i32), Thrown> {
        if width <= 0 || height <= 0 {
            return Err(Thrown);
        }
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel(format)))
            .ok_or(Thrown)?;
        let mut drawing = self.drawing();
        if drawing.locked.is_some() {
            return Err(Thrown);
        }
        let mut pixels = std::mem::take(&mut drawing.spare);
        pixels.resize(size, 0);
        let bits = pixels.as_mut_ptr();
        drawing.locked = Some(Frame {
            pixels,
            width,
            height,
            format,
        });
        Ok((bits, width, height, format))
    }

    /// Posts what the last lock handed out.
    pub fn post(&self, jni: Jni) -> Result<(), Thrown> {
        let mut drawing = self.drawing();
        let Some(mut frame) = drawing.locked.take() else {
            return Err(Thrown);
        };
        let result = self.draw(jni, &mut drawing, &mut frame);
        drawing.spare = frame.pixels;
        result
    }

    fn draw(&self, jni: Jni, drawing: &mut Drawing, frame: &mut Frame) -> Result<(), Thrown> {
        let config = if frame.format == FORMAT_RGB_565 {
            c"RGB_565"
        } else {
            c"ARGB_8888"
        };
        let reusable = drawing
            .bitmap
            .is_some_and(|(_, w, h, f)| (w, h, f) == (frame.width, frame.height, frame.format));
        if !reusable {
            if let Some((old, ..)) = drawing.bitmap.take() {
                jni.delete_global(old);
            }
            let bitmap = new_bitmap(jni, frame.width, frame.height, config)?;
            drawing.bitmap = Some((jni.global(bitmap), frame.width, frame.height, frame.format));
            jni.delete_local(bitmap);
        }
        let Some((bitmap, ..)) = drawing.bitmap else {
            return Err(Thrown);
        };
        let layout = Layout {
            width: frame.width,
            height: frame.height,
            stride: frame.width as usize * bytes_per_pixel(frame.format),
            rgb565: frame.format == FORMAT_RGB_565,
            // A window's RGBA pixels are premultiplied, as the compositor blends them.
            premultiplied: true,
            opaque: frame.format == FORMAT_RGBX_8888,
        };
        // SAFETY: live references of this thread and the framework's signatures.
        unsafe {
            transfer(jni, bitmap, layout, &mut frame.pixels, true)?;
            let canvas = jni
                .call(
                    self.surface,
                    c"lockCanvas",
                    c"(Landroid/graphics/Rect;)Landroid/graphics/Canvas;",
                    &[jvalue {
                        l: std::ptr::null_mut(),
                    }],
                )?
                .l;
            let drawn = self.draw_scaled(jni, canvas, bitmap, frame.width, frame.height);
            let posted = jni.call(
                self.surface,
                c"unlockCanvasAndPost",
                c"(Landroid/graphics/Canvas;)V",
                &[jvalue { l: canvas }],
            );
            jni.delete_local(canvas);
            drawn?;
            posted?;
        }
        Ok(())
    }

    /// Draws the buffer over the whole Surface, scaled when the app chose another size, as Android
    /// scales a buffer to its window.
    ///
    /// # Safety
    /// `canvas` and `bitmap` must be live references of this thread.
    unsafe fn draw_scaled(
        &self,
        jni: Jni,
        canvas: jobject,
        bitmap: jobject,
        width: i32,
        height: i32,
    ) -> Result<(), Thrown> {
        // SAFETY: as above, with the framework's signatures.
        unsafe {
            let target_width = jni.call(canvas, c"getWidth", c"()I", &[])?.i;
            let target_height = jni.call(canvas, c"getHeight", c"()I", &[])?.i;
            if (target_width, target_height) == (width, height) {
                let args = [
                    jvalue { l: bitmap },
                    jvalue { f: 0.0 },
                    jvalue { f: 0.0 },
                    jvalue {
                        l: std::ptr::null_mut(),
                    },
                ];
                jni.call(
                    canvas,
                    c"drawBitmap",
                    c"(Landroid/graphics/Bitmap;FFLandroid/graphics/Paint;)V",
                    &args,
                )?;
                return Ok(());
            }
            let rect = |w: i32, h: i32| {
                let args = [0, 0, w, h].map(|i| jvalue { i });
                jni.new_object(c"android/graphics/Rect", c"(IIII)V", &args)
            };
            let source = rect(width, height)?;
            let target = rect(target_width, target_height);
            let drawn = target.and_then(|target| {
                let args = [jvalue { l: bitmap }, jvalue { l: source }, jvalue { l: target }, jvalue { l: std::ptr::null_mut() }];
                let drawn = jni.call(
                    canvas,
                    c"drawBitmap",
                    c"(Landroid/graphics/Bitmap;Landroid/graphics/Rect;Landroid/graphics/Rect;Landroid/graphics/Paint;)V",
                    &args,
                );
                jni.delete_local(target);
                drawn
            });
            jni.delete_local(source);
            drawn.map(|_| ())
        }
    }
}

/// `ANativeWindow* ANativeWindow_fromSurface(JNIEnv*, jobject surface)`, with one reference.
///
/// # Safety
/// The arguments must meet the contract of the NDK function of the same name.
pub unsafe extern "C" fn from_surface(env: *mut JNIEnv, surface: jobject) -> *mut c_void {
    if env.is_null() || surface.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: app code passes its thread's environment.
    let jni = unsafe { Jni::from_env(env) };
    // SAFETY: as above, with a live reference; `isValid` returns a boolean.
    let valid = unsafe {
        jni.call(surface, c"isValid", c"()Z", &[])
            .is_ok_and(|value| value.z)
    };
    if !valid {
        return std::ptr::null_mut();
    }
    WINDOWS.add(Arc::new(Window {
        surface: jni.global(surface),
        references: AtomicUsize::new(1),
        geometry: Mutex::new(Geometry::default()),
        drawing: Mutex::new(Drawing {
            locked: None,
            spare: Vec::new(),
            bitmap: None,
        }),
    }))
}

/// `jobject ANativeWindow_toSurface(JNIEnv*, ANativeWindow*)`: a local reference to the Surface.
pub unsafe extern "C" fn to_surface(env: *mut JNIEnv, window: *mut c_void) -> jobject {
    let Some(window) = WINDOWS.get(window) else {
        return std::ptr::null_mut();
    };
    if env.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: app code passes its thread's environment, and the reference is global.
    unsafe { ((**env).v1_2.NewLocalRef)(env, window.surface) }
}

pub unsafe extern "C" fn acquire(window: *mut c_void) {
    if let Some(window) = WINDOWS.get(window) {
        window.references.fetch_add(1, Ordering::Relaxed);
    }
}

///
/// # Safety
/// The arguments must meet the contract of the NDK function of the same name.
pub unsafe extern "C" fn release(pointer: *mut c_void) {
    let Some(window) = WINDOWS.get(pointer) else {
        return;
    };
    if window.references.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    WINDOWS.remove(pointer);
    // Without a Java thread to delete them on, the references stay: a leak, never a crash.
    if let Some(jni) = Jni::current() {
        let mut drawing = window.drawing();
        if let Some((bitmap, ..)) = drawing.bitmap.take() {
            jni.delete_global(bitmap);
        }
        jni.delete_global(window.surface);
    }
}

fn with_window<T>(
    pointer: *mut c_void,
    failed: T,
    action: impl FnOnce(Jni, &Window) -> Result<T, Thrown>,
) -> T {
    let Some(window) = WINDOWS.get(pointer) else {
        return failed;
    };
    let Some(jni) = Jni::current() else {
        return failed;
    };
    action(jni, &window).unwrap_or(failed)
}

pub unsafe extern "C" fn get_width(window: *mut c_void) -> i32 {
    with_window(window, -libc::EINVAL, |jni, window| {
        Ok(window.buffer_size(jni)?.0)
    })
}

pub unsafe extern "C" fn get_height(window: *mut c_void) -> i32 {
    with_window(window, -libc::EINVAL, |jni, window| {
        Ok(window.buffer_size(jni)?.1)
    })
}

pub unsafe extern "C" fn get_format(window: *mut c_void) -> i32 {
    match WINDOWS.get(window) {
        Some(window) => match window.geometry().format {
            0 => FORMAT_RGBA_8888,
            format => format,
        },
        None => -libc::EINVAL,
    }
}

/// `int32_t ANativeWindow_setBuffersGeometry(ANativeWindow*, int32_t width, int32_t height,
/// int32_t format)`: zeros keep the window's size and format.
pub unsafe extern "C" fn set_buffers_geometry(
    window: *mut c_void,
    width: i32,
    height: i32,
    format: i32,
) -> i32 {
    let Some(window) = WINDOWS.get(window) else {
        return -libc::EINVAL;
    };
    let supported = [0, FORMAT_RGBA_8888, FORMAT_RGBX_8888, FORMAT_RGB_565];
    if width < 0 || height < 0 || (width == 0) != (height == 0) || !supported.contains(&format) {
        return -libc::EINVAL;
    }
    *window.geometry.lock().unwrap_or_else(|p| p.into_inner()) = Geometry {
        width,
        height,
        format,
    };
    0
}

/// `int32_t ANativeWindow_lock(ANativeWindow*, ANativeWindow_Buffer* outBuffer, ARect* inOutDirtyBounds)`.
pub unsafe extern "C" fn lock(window: *mut c_void, out: *mut Buffer, dirty: *mut Rect) -> i32 {
    if out.is_null() {
        return -libc::EINVAL;
    }
    with_window(window, -libc::EINVAL, |jni, window| {
        let (bits, width, height, format) = window.lock(jni)?;
        // SAFETY: both pointers are null or writable, by the NDK contract; `out` was checked.
        unsafe {
            out.write(Buffer {
                width,
                height,
                stride: width,
                format,
                bits: bits.cast(),
                reserved: [0; 6],
            });
            // Every post copies the whole buffer, so the whole buffer is dirty.
            if !dirty.is_null() {
                dirty.write(Rect {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                });
            }
        }
        Ok(0)
    })
}

pub unsafe extern "C" fn unlock_and_post(window: *mut c_void) -> i32 {
    with_window(window, -libc::EINVAL, |jni, window| {
        window.post(jni).map(|()| 0)
    })
}
