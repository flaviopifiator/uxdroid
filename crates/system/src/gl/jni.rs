//! What the GL natives share: the host's GL functions, and Java arrays, buffers and strings as the C
//! pointers GL takes, each checked to hold what the call reads or writes.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_void};
use std::sync::OnceLock;

use jni_sys::{JNIEnv, jarray, jfieldID, jint, jmethodID, jobject, jstring};

pub(super) use crate::Method;

pub(super) const GL_COLOR: u32 = 0x1800;
pub(super) const GL_CURRENT_VERTEX_ATTRIB: u32 = 0x8626;
pub(super) const GL_TEXTURE_BORDER_COLOR: u32 = 0x1004;

/// Throws `class` with `message`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
pub(super) unsafe fn throw(env: *mut JNIEnv, class: &CStr, message: &str) {
    // SAFETY: as above.
    unsafe { crate::throw(env, class, message) };
}

/// Throws UnsupportedOperationException naming a native nobody implements: AOSP declares a few
/// overloads whose last argument is one byte where a buffer is meant, which nothing can call usefully.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
pub(super) unsafe fn unsupported(env: *mut JNIEnv, method: &CStr) {
    let message = format!(
        "android.opengl {} is not implemented",
        method.to_string_lossy()
    );
    // SAFETY: as above.
    unsafe { throw(env, c"java/lang/UnsupportedOperationException", &message) };
}

/// The host's GL function `name`, looked up once into `address`, or None with
/// UnsupportedOperationException thrown when the host's GLES lacks it.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `F` the function's C signature.
pub(super) unsafe fn gl<F: Copy>(
    env: *mut JNIEnv,
    address: &OnceLock<usize>,
    name: &CStr,
) -> Option<F> {
    const { assert!(size_of::<F>() == size_of::<usize>()) };
    let found = *address.get_or_init(|| {
        uxdroid_ndk::host_gl::gles_symbol(name).map_or(0, |address| address as usize)
    });
    if found == 0 {
        let message = format!("{} is not in the host's GLES", name.to_string_lossy());
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/UnsupportedOperationException", &message) };
        return None;
    }
    // SAFETY: as above; a function pointer has the size of usize, checked at compile time.
    Some(unsafe { std::mem::transmute_copy::<usize, F>(&found) })
}

/// Memory of a Java array or buffer for one GL call. An array is pinned only when the call takes its
/// pointer, after every other JNI call of the native, and released when this drops.
pub(super) struct Pinned {
    env: *mut JNIEnv,
    array: jarray,
    /// Bytes from the start of the array, or the address itself when there is no array.
    offset: usize,
    /// Bytes from the pointer to the buffer's limit; 0 for an array.
    pub(super) remaining: usize,
    pinned: Cell<*mut c_void>,
}

impl Pinned {
    fn address(address: *mut c_void) -> Self {
        Self {
            env: std::ptr::null_mut(),
            array: std::ptr::null_mut(),
            offset: address as usize,
            remaining: 0,
            pinned: Cell::new(std::ptr::null_mut()),
        }
    }

    /// The pointer GL takes.
    pub(super) fn pointer(&self) -> *mut c_void {
        if self.array.is_null() {
            return self.offset as *mut c_void;
        }
        // SAFETY: a live array of this call; the region ends when this drops, before any other JNI
        // call of the native.
        let base = unsafe {
            ((**self.env).v1_2.GetPrimitiveArrayCritical)(
                self.env,
                self.array,
                std::ptr::null_mut(),
            )
        };
        self.pinned.set(base);
        if base.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the offset was checked against the array's length.
        unsafe { base.cast::<u8>().add(self.offset).cast() }
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        let base = self.pinned.get();
        if !base.is_null() {
            // SAFETY: pinned in `pointer` from this array; mode 0 copies back and frees, which GL's
            // writes need.
            unsafe {
                ((**self.env).v1_2.ReleasePrimitiveArrayCritical)(self.env, self.array, base, 0)
            };
        }
    }
}

/// `array` from element `offset` on, which must hold `needed` elements of `size` bytes.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `array` a primitive array or null.
pub(super) unsafe fn array(
    env: *mut JNIEnv,
    array: jarray,
    offset: jint,
    needed: usize,
    size: usize,
    function: &CStr,
) -> Option<Pinned> {
    let refuse = |message: &str| {
        let message = format!("{}: {message}", function.to_string_lossy());
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalArgumentException", &message) };
        None
    };
    if array.is_null() {
        return refuse("array == null");
    }
    if offset < 0 {
        return refuse("offset < 0");
    }
    // SAFETY: as above.
    let length = unsafe { ((**env).v1_1.GetArrayLength)(env, array) };
    let remaining = (length - offset.min(length)) as usize;
    if offset > length || remaining < needed {
        return refuse("length - offset < needed");
    }
    Some(Pinned {
        env,
        array,
        offset: offset as usize * size,
        remaining: 0,
        pinned: Cell::new(std::ptr::null_mut()),
    })
}

struct BufferIds {
    position: jfieldID,
    limit: jfieldID,
    shift: jfieldID,
    is_direct: jmethodID,
    array: jmethodID,
    array_offset: jmethodID,
}

// SAFETY: field and method ids stay valid while the class is loaded, which java.nio.Buffer always is.
unsafe impl Send for BufferIds {}
unsafe impl Sync for BufferIds {}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn buffer_ids(env: *mut JNIEnv) -> Option<&'static BufferIds> {
    static IDS: OnceLock<Option<BufferIds>> = OnceLock::new();
    IDS.get_or_init(|| {
        // SAFETY: as above; java.nio.Buffer is in the boot class path, with these members in ART's
        // libcore.
        unsafe {
            let jni = &**env;
            let class = (jni.v1_1.FindClass)(env, c"java/nio/Buffer".as_ptr());
            if class.is_null() {
                return None;
            }
            let field =
                |name: &CStr| (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), c"I".as_ptr());
            let method = |name: &CStr, signature: &CStr| {
                (jni.v1_1.GetMethodID)(env, class, name.as_ptr(), signature.as_ptr())
            };
            let ids = BufferIds {
                position: field(c"position"),
                limit: field(c"limit"),
                shift: field(c"_elementSizeShift"),
                is_direct: method(c"isDirect", c"()Z"),
                array: method(c"array", c"()Ljava/lang/Object;"),
                array_offset: method(c"arrayOffset", c"()I"),
            };
            let complete = !ids.position.is_null()
                && !ids.limit.is_null()
                && !ids.shift.is_null()
                && !ids.is_direct.is_null()
                && !ids.array.is_null()
                && !ids.array_offset.is_null();
            complete.then_some(ids)
        }
    })
    .as_ref()
}

/// A java.nio buffer from its position on, which must hold `needed` bytes; null is accepted, as a
/// null pointer, only when `nullable`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `buffer` a java.nio.Buffer or null.
pub(super) unsafe fn buffer(
    env: *mut JNIEnv,
    buffer: jobject,
    needed: usize,
    nullable: bool,
    function: &CStr,
) -> Option<Pinned> {
    let refuse = |message: &str| {
        let message = format!("{}: {message}", function.to_string_lossy());
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalArgumentException", &message) };
        None
    };
    if buffer.is_null() {
        return if nullable {
            Some(Pinned::address(std::ptr::null_mut()))
        } else {
            refuse("buffer == null")
        };
    }
    // SAFETY: as above.
    let Some(ids) = (unsafe { buffer_ids(env) }) else {
        return refuse("java.nio.Buffer has an unexpected shape");
    };
    // SAFETY: as above; the ids belong to java.nio.Buffer, which `buffer` is.
    unsafe {
        let jni = &**env;
        let position = (jni.v1_1.GetIntField)(env, buffer, ids.position);
        let limit = (jni.v1_1.GetIntField)(env, buffer, ids.limit);
        let shift = (jni.v1_1.GetIntField)(env, buffer, ids.shift);
        if !(0..=3).contains(&shift) || position < 0 || limit < position {
            return refuse("the buffer's state is not valid");
        }
        if (((limit - position) as usize) << shift) < needed {
            return refuse("remaining() < needed");
        }
        let start = (position as usize) << shift;
        if (jni.v1_1.CallBooleanMethodA)(env, buffer, ids.is_direct, std::ptr::null()) {
            let address = (jni.v1_4.GetDirectBufferAddress)(env, buffer);
            if address.is_null() {
                return refuse("the direct buffer has no address");
            }
            let mut pinned = Pinned::address(address.cast::<u8>().add(start).cast());
            pinned.remaining = ((limit - position) as usize) << shift;
            return Some(pinned);
        }
        let array = (jni.v1_1.CallObjectMethodA)(env, buffer, ids.array, std::ptr::null());
        if array.is_null() || (jni.v1_2.ExceptionCheck)(env) {
            // A read-only heap buffer refuses array(); its exception stays thrown.
            return None;
        }
        let offset = (jni.v1_1.CallIntMethodA)(env, buffer, ids.array_offset, std::ptr::null());
        Some(Pinned {
            env,
            array,
            offset: ((offset.max(0) as usize) << shift) + start,
            remaining: ((limit - position) as usize) << shift,
            pinned: Cell::new(std::ptr::null_mut()),
        })
    }
}

/// A Java string as a C string, or None with IllegalArgumentException thrown for null.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `text` a string or null.
pub(super) unsafe fn string(env: *mut JNIEnv, text: jstring, function: &CStr) -> Option<CString> {
    // SAFETY: as above.
    match unsafe { crate::java_string(env, text) } {
        Some(text) => Some(CString::new(text.replace('\0', "")).unwrap_or_default()),
        None => {
            let message = format!("{}: string == null", function.to_string_lossy());
            // SAFETY: as above.
            unsafe { throw(env, c"java/lang/IllegalArgumentException", &message) };
            None
        }
    }
}

type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);

fn get_integer(name: u32) -> i32 {
    static ADDRESS: OnceLock<usize> = OnceLock::new();
    let address = *ADDRESS.get_or_init(|| {
        uxdroid_ndk::host_gl::gles_symbol(c"glGetIntegerv").map_or(0, |address| address as usize)
    });
    if address == 0 {
        return 0;
    }
    let mut value = 0;
    // SAFETY: the host's glGetIntegerv has this signature; it writes one value for these names.
    unsafe { std::mem::transmute::<usize, GetIntegerv>(address)(name, &mut value) };
    value
}

/// How many values glGet* writes for `name`: most write one, these more, as the GLES 3.0
/// specification's state tables give them.
pub(super) fn get_needed(name: u32) -> usize {
    match name {
        // GL_DEPTH_RANGE, GL_ALIASED_POINT_SIZE_RANGE, GL_ALIASED_LINE_WIDTH_RANGE,
        // GL_MAX_VIEWPORT_DIMS.
        0x0B70 | 0x846D | 0x846E | 0x0D3A => 2,
        // GL_VIEWPORT, GL_SCISSOR_BOX, GL_COLOR_CLEAR_VALUE, GL_COLOR_WRITEMASK, GL_BLEND_COLOR.
        0x0BA2 | 0x0C10 | 0x0C22 | 0x0C23 | 0x8005 => 4,
        // GL_COMPRESSED_TEXTURE_FORMATS, by GL_NUM_COMPRESSED_TEXTURE_FORMATS.
        0x86A3 => get_integer(0x86A2).max(0) as usize,
        // GL_SHADER_BINARY_FORMATS, by GL_NUM_SHADER_BINARY_FORMATS.
        0x8DF8 => get_integer(0x8DF9).max(0) as usize,
        // GL_PROGRAM_BINARY_FORMATS, by GL_NUM_PROGRAM_BINARY_FORMATS.
        0x87FF => get_integer(0x87FE).max(0) as usize,
        _ => 1,
    }
}

/// Bytes glReadPixels writes for a `width` by `height` rectangle, with the current pack alignment.
/// A format or type this does not know counts 16 bytes a pixel, the most any takes.
pub(super) fn pixel_bytes(width: i32, height: i32, format: u32, kind: u32) -> usize {
    if width <= 0 || height <= 0 {
        return 0;
    }
    let components = match format {
        0x1903 | 0x1906 | 0x1909 | 0x8D94 | 0x1902 => Some(1), // RED, ALPHA, LUMINANCE, RED_INTEGER, DEPTH
        0x190A | 0x8227 | 0x8228 => Some(2),                   // LUMINANCE_ALPHA, RG, RG_INTEGER
        0x1907 | 0x8D98 => Some(3),                            // RGB, RGB_INTEGER
        0x1908 | 0x8D99 => Some(4),                            // RGBA, RGBA_INTEGER
        _ => None,
    };
    let pixel = match (kind, components) {
        (0x1401 | 0x1400, Some(n)) => n, // UNSIGNED_BYTE, BYTE
        (0x1403 | 0x1402 | 0x8D61 | 0x140B, Some(n)) => 2 * n, // (UNSIGNED_)SHORT, HALF_FLOAT(_OES)
        (0x1404..=0x1406, Some(n)) => 4 * n, // (UNSIGNED_)INT, FLOAT
        (0x8363 | 0x8033 | 0x8034, _) => 2, // 5_6_5, 4_4_4_4, 5_5_5_1
        (0x8368 | 0x8C3B | 0x8C3E | 0x84FA, _) => 4, // 2_10_10_10_REV, 10F_11F_11F_REV, 5_9_9_9_REV, 24_8
        _ => 16,
    };
    let row = width as usize * pixel;
    // GL_PACK_ALIGNMENT: 1, 2, 4 or 8.
    let alignment = match get_integer(0x0D05) {
        a @ (1 | 2 | 4 | 8) => a as usize,
        _ => 4,
    };
    let stride = row.div_ceil(alignment) * alignment;
    stride * (height as usize - 1) + row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_valued_state_needs_room_for_every_value() {
        assert_eq!(get_needed(0x0BA2), 4, "GL_VIEWPORT");
        assert_eq!(get_needed(0x0B70), 2, "GL_DEPTH_RANGE");
        assert_eq!(get_needed(0x0D33), 1, "GL_MAX_TEXTURE_SIZE");
    }

    #[test]
    fn read_pixels_needs_every_row_but_the_last_padded_to_the_alignment() {
        // Without a GL context, the alignment is the default 4.
        assert_eq!(pixel_bytes(2, 2, 0x1908, 0x1401), 16, "RGBA bytes");
        assert_eq!(
            pixel_bytes(3, 2, 0x1907, 0x1401),
            12 + 9,
            "RGB rows padded to 12"
        );
        assert_eq!(pixel_bytes(1, 1, 0x1908, 0x1406), 16, "RGBA floats");
        assert_eq!(pixel_bytes(0, 5, 0x1908, 0x1401), 0);
        assert_eq!(
            pixel_bytes(2, 1, 0xFFFF, 0x1401),
            32,
            "an unknown format counts 16 bytes a pixel"
        );
    }
}
