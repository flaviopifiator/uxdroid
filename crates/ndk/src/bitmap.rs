//! `libjnigraphics`: `AndroidBitmap_*` over the framework's `android.graphics.Bitmap`.
//!
//! The pixels live in AOSP's host graphics library, which exports no C API for them. Locking copies
//! them out with `Bitmap.getPixels` into Android's memory layout, and unlocking copies them back with
//! `setPixels`, which also tells the framework they changed. See `pixels` for the layouts. Formats
//! other than `RGBA_8888` and `RGB_565` report their info but do not lock.

use std::collections::HashMap;
use std::ffi::{CStr, c_int, c_void};
use std::sync::Mutex;

use jni_sys::{JNIEnv, jobject, jvalue};

use crate::java::{Jni, Thrown};
use crate::pixels::{colors_to_memory, memory_to_colors};

const RESULT_SUCCESS: c_int = 0;
const RESULT_BAD_PARAMETER: c_int = -1;
const RESULT_JNI_EXCEPTION: c_int = -2;
pub const FORMAT_RGBA_8888: i32 = 1;
pub const FORMAT_RGB_565: i32 = 4;
const FORMAT_RGBA_4444: i32 = 7;
const FORMAT_A_8: i32 = 8;
const FORMAT_RGBA_F16: i32 = 9;
const FORMAT_RGBA_1010102: i32 = 10;
const FLAGS_ALPHA_PREMUL: u32 = 0;
const FLAGS_ALPHA_OPAQUE: u32 = 1;
const FLAGS_ALPHA_UNPREMUL: u32 = 2;
const FLAGS_IS_HARDWARE: u32 = 1 << 31;

/// `AndroidBitmapInfo` from `<android/bitmap.h>`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Info {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

struct Locked {
    pixels: Box<[u8]>,
    count: usize,
    info: Info,
}

/// Locked bitmaps by the framework's native Bitmap handle, which identifies a Bitmap on any thread.
static LOCKED: Mutex<Option<HashMap<i64, Locked>>> = Mutex::new(None);

/// A mutable Bitmap of `config`, a `Bitmap.Config` constant's name.
pub fn new_bitmap(jni: Jni, width: i32, height: i32, config: &CStr) -> Result<jobject, Thrown> {
    let bitmap_class = jni.find_class(c"android/graphics/Bitmap")?;
    let config_class = jni.find_class(c"android/graphics/Bitmap$Config");
    // SAFETY: live classes and the framework's signatures.
    let bitmap = config_class.and_then(|config_class| unsafe {
        let config = jni.static_object(config_class, config, c"Landroid/graphics/Bitmap$Config;");
        jni.delete_local(config_class);
        let config = config?;
        let made = jni.call_static(
            bitmap_class,
            c"createBitmap",
            c"(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;",
            &[
                jvalue { i: width },
                jvalue { i: height },
                jvalue { l: config },
            ],
        );
        jni.delete_local(config);
        made.map(|value| value.l)
    });
    jni.delete_local(bitmap_class);
    bitmap.and_then(|bitmap| {
        if bitmap.is_null() {
            Err(Thrown)
        } else {
            Ok(bitmap)
        }
    })
}

/// How pixels lie in memory.
#[derive(Clone, Copy)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    /// Bytes from one row to the next.
    pub stride: usize,
    pub rgb565: bool,
    pub premultiplied: bool,
    /// The alpha bytes are padding, as in `RGBX_8888`.
    pub opaque: bool,
}

/// Copies a Bitmap's pixels between Java and Android's memory layout, through `getPixels` or
/// `setPixels` and one int array.
///
/// # Safety
/// `bitmap` must be a live Bitmap reference of this thread.
pub unsafe fn transfer(
    jni: Jni,
    bitmap: jobject,
    layout: Layout,
    memory: &mut [u8],
    to_java: bool,
) -> Result<(), Thrown> {
    let Layout {
        width,
        height,
        stride,
        rgb565,
        premultiplied,
        opaque,
    } = layout;
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    let count = w.checked_mul(h).ok_or(Thrown)?;
    let bytes = if rgb565 { 2 } else { 4 };
    if stride < w * bytes || memory.len() < stride * h.saturating_sub(1) + w * bytes {
        return Err(Thrown);
    }
    let array = jni.int_array(count)?;
    let mut colors = vec![0i32; count];
    let args = [
        jvalue { l: array },
        jvalue { i: 0 },
        jvalue { i: width },
        jvalue { i: 0 },
        jvalue { i: 0 },
        jvalue { i: width },
        jvalue { i: height },
    ];
    let result = if to_java {
        memory_to_colors(
            memory,
            stride,
            &mut colors,
            w,
            rgb565,
            premultiplied,
            opaque || rgb565,
        );
        jni.write_ints(array, &colors).and_then(|()| {
            // SAFETY: as above; the array holds width * height colors.
            unsafe { jni.call(bitmap, c"setPixels", c"([IIIIIII)V", &args) }.map(|_| ())
        })
    } else {
        // SAFETY: as above.
        unsafe { jni.call(bitmap, c"getPixels", c"([IIIIIII)V", &args) }
            .and_then(|_| jni.read_ints(array, &mut colors))
            .map(|()| colors_to_memory(&colors, w, memory, stride, rgb565, premultiplied))
    };
    jni.delete_local(array);
    result
}

fn info(jni: Jni, bitmap: jobject) -> Result<Info, Thrown> {
    // SAFETY: a live Bitmap and the framework's signatures.
    unsafe {
        let width = jni.call(bitmap, c"getWidth", c"()I", &[])?.i;
        let height = jni.call(bitmap, c"getHeight", c"()I", &[])?.i;
        let stride = jni.call(bitmap, c"getRowBytes", c"()I", &[])?.i;
        let config = jni
            .call(
                bitmap,
                c"getConfig",
                c"()Landroid/graphics/Bitmap$Config;",
                &[],
            )?
            .l;
        let name = jni.call(config, c"name", c"()Ljava/lang/String;", &[]);
        jni.delete_local(config);
        let name = name?.l;
        let text = jni.utf(name).unwrap_or_default();
        jni.delete_local(name);
        let (format, hardware) = match text.as_slice() {
            b"ARGB_8888" => (FORMAT_RGBA_8888, false),
            b"RGB_565" => (FORMAT_RGB_565, false),
            b"ARGB_4444" => (FORMAT_RGBA_4444, false),
            b"ALPHA_8" => (FORMAT_A_8, false),
            b"RGBA_F16" => (FORMAT_RGBA_F16, false),
            b"RGBA_1010102" => (FORMAT_RGBA_1010102, false),
            b"HARDWARE" => (FORMAT_RGBA_8888, true),
            _ => (0, false),
        };
        let alpha = if !jni.call(bitmap, c"hasAlpha", c"()Z", &[])?.z {
            FLAGS_ALPHA_OPAQUE
        } else if jni.call(bitmap, c"isPremultiplied", c"()Z", &[])?.z {
            FLAGS_ALPHA_PREMUL
        } else {
            FLAGS_ALPHA_UNPREMUL
        };
        Ok(Info {
            width: width.max(0) as u32,
            height: height.max(0) as u32,
            stride: stride.max(0) as u32,
            format,
            flags: alpha | if hardware { FLAGS_IS_HARDWARE } else { 0 },
        })
    }
}

/// `int AndroidBitmap_getInfo(JNIEnv*, jobject bitmap, AndroidBitmapInfo* info)`.
pub unsafe extern "C" fn get_info(env: *mut JNIEnv, bitmap: jobject, out: *mut Info) -> c_int {
    if env.is_null() || bitmap.is_null() {
        return RESULT_BAD_PARAMETER;
    }
    // SAFETY: app code passes its thread's environment.
    let jni = unsafe { Jni::from_env(env) };
    match info(jni, bitmap) {
        Ok(info) => {
            if !out.is_null() {
                // SAFETY: the NDK contract makes a non-null `out` writable.
                unsafe { out.write(info) };
            }
            RESULT_SUCCESS
        }
        Err(Thrown) => RESULT_JNI_EXCEPTION,
    }
}

fn handle(jni: Jni, bitmap: jobject) -> Result<i64, Thrown> {
    // SAFETY: a live Bitmap; `getNativeInstance` is public in the framework.
    unsafe { Ok(jni.call(bitmap, c"getNativeInstance", c"()J", &[])?.j) }
}

fn locked() -> std::sync::MutexGuard<'static, Option<HashMap<i64, Locked>>> {
    LOCKED.lock().unwrap_or_else(|p| p.into_inner())
}

fn layout(info: &Info) -> Layout {
    Layout {
        width: info.width as i32,
        height: info.height as i32,
        stride: info.stride as usize,
        rgb565: info.format == FORMAT_RGB_565,
        premultiplied: info.flags & 3 == FLAGS_ALPHA_PREMUL,
        opaque: false,
    }
}

fn lockable(info: &Info) -> bool {
    info.flags & FLAGS_IS_HARDWARE == 0 && matches!(info.format, FORMAT_RGBA_8888 | FORMAT_RGB_565)
}

/// `int AndroidBitmap_lockPixels(JNIEnv*, jobject bitmap, void** addrPtr)`.
pub unsafe extern "C" fn lock_pixels(
    env: *mut JNIEnv,
    bitmap: jobject,
    address: *mut *mut c_void,
) -> c_int {
    if env.is_null() || bitmap.is_null() {
        return RESULT_BAD_PARAMETER;
    }
    // SAFETY: app code passes its thread's environment.
    let jni = unsafe { Jni::from_env(env) };
    let lock = || -> Result<Option<*mut c_void>, Thrown> {
        let key = handle(jni, bitmap)?;
        if let Some(entry) = locked().get_or_insert_with(HashMap::new).get_mut(&key) {
            entry.count += 1;
            return Ok(Some(entry.pixels.as_mut_ptr().cast()));
        }
        let info = info(jni, bitmap)?;
        if !lockable(&info) {
            return Ok(None);
        }
        let size = (info.stride as usize)
            .checked_mul(info.height as usize)
            .ok_or(Thrown)?;
        let mut pixels = vec![0u8; size].into_boxed_slice();
        // SAFETY: a live Bitmap of this thread.
        unsafe { transfer(jni, bitmap, layout(&info), &mut pixels, false)? };
        let pointer = pixels.as_mut_ptr().cast();
        locked().get_or_insert_with(HashMap::new).insert(
            key,
            Locked {
                pixels,
                count: 1,
                info,
            },
        );
        Ok(Some(pointer))
    };
    match lock() {
        Ok(Some(pointer)) => {
            if !address.is_null() {
                // SAFETY: the NDK contract makes a non-null `address` writable.
                unsafe { *address = pointer };
            }
            RESULT_SUCCESS
        }
        Ok(None) => RESULT_BAD_PARAMETER,
        Err(Thrown) => RESULT_JNI_EXCEPTION,
    }
}

/// `int AndroidBitmap_unlockPixels(JNIEnv*, jobject bitmap)`: writes app changes back.
pub unsafe extern "C" fn unlock_pixels(env: *mut JNIEnv, bitmap: jobject) -> c_int {
    if env.is_null() || bitmap.is_null() {
        return RESULT_BAD_PARAMETER;
    }
    // SAFETY: app code passes its thread's environment.
    let jni = unsafe { Jni::from_env(env) };
    let Ok(key) = handle(jni, bitmap) else {
        return RESULT_JNI_EXCEPTION;
    };
    let mut entry = {
        let mut guard = locked();
        let map = guard.get_or_insert_with(HashMap::new);
        match map.get_mut(&key) {
            None => return RESULT_BAD_PARAMETER,
            Some(entry) if entry.count > 1 => {
                entry.count -= 1;
                return RESULT_SUCCESS;
            }
            Some(_) => match map.remove(&key) {
                Some(entry) => entry,
                None => return RESULT_BAD_PARAMETER,
            },
        }
    };
    let info = entry.info;
    // SAFETY: a live Bitmap of this thread; writing to an immutable one is the app's mistake.
    let written = unsafe {
        if !jni
            .call(bitmap, c"isMutable", c"()Z", &[])
            .is_ok_and(|value| value.z)
        {
            return RESULT_SUCCESS;
        }
        transfer(jni, bitmap, layout(&info), &mut entry.pixels, true)
    };
    match written {
        Ok(()) => RESULT_SUCCESS,
        Err(Thrown) => RESULT_JNI_EXCEPTION,
    }
}
