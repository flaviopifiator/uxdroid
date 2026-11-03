//! The framework's Java GL bindings: `android.opengl.GLES20` and `GLES30`, and the EGL10 of
//! `javax.microedition.khronos`, `com.google.android.gles_jni.EGLImpl`, which `GLSurfaceView`
//! uses. On Android they are JNI code in `libandroid_runtime`, which AOSP's host build leaves out,
//! so every one had a default body: Mindustry's renderer found no EGL config and stopped with
//! "Arc requires OpenGL ES 2.0".
//!
//! Most GLES natives are generated (`gles.rs`, from `gles_jni.py`); those that return a String or a
//! buffer, or take an array of strings, are here. EGL goes to the same functions app native code
//! reaches through `libEGL`'s table: Mesa's surfaceless display, and a window surface on a `Surface`
//! is a pbuffer read back into it at each swap (`uxdroid_ndk::host_gl`). The GL10 of `GLImpl`
//! answers the functions GLES shares with it, such as glGetString; its fixed-function rest,
//! `android.opengl.EGL14`, `GLES31` and `GLES32` keep their default bodies.

mod egl10;
#[rustfmt::skip]
mod gles;
mod jni;

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use jni_sys::{JNIEnv, jarray, jclass, jint, jobject, jobjectArray, jstring};

use crate::Method;
use jni::{array, buffer, gl, string, throw};

/// Registers the natives on their classes, which are loaded but not initialized, so nothing runs
/// their static initializers at startup.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
pub(crate) unsafe fn register(env: *mut JNIEnv) -> Result<(), String> {
    let gles20: Vec<Method> = gles::GLES20
        .iter()
        .copied()
        .chain(HAND_GLES20.iter().copied())
        .collect();
    let gles30: Vec<Method> = gles::GLES30
        .iter()
        .copied()
        .chain(HAND_GLES30.iter().copied())
        .collect();
    // GLImpl's methods that GLES has too, such as glGetString, which libGDX's renderers call on the
    // GL10 they are handed; its fixed-function rest keeps the default bodies.
    let gl10: Vec<Method> = gles::GL_IMPL
        .iter()
        .filter_map(|(name, descriptor)| {
            gles20
                .iter()
                .chain(&gles30)
                .chain(HAND_GL_IMPL)
                .find(|(other, signature, _)| other == name && signature == descriptor)
                .copied()
        })
        .collect();
    // SAFETY: as above; each table matches the framework's declarations in Android 16.
    unsafe {
        crate::register_uninitialized(env, c"android.opengl.GLES20", &gles20)?;
        crate::register_uninitialized(env, c"android.opengl.GLES30", &gles30)?;
        crate::register_uninitialized(env, c"com.google.android.gles_jni.EGLImpl", egl10::METHODS)?;
        crate::register_uninitialized(env, c"com.google.android.gles_jni.GLImpl", &gl10)
    }
}

unsafe extern "system" fn class_init(_env: *mut JNIEnv, _class: jclass) {}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn new_string(env: *mut JNIEnv, text: *const c_char) -> jstring {
    if text.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: as above; GL returns a C string.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    // SAFETY: as above.
    unsafe { crate::new_string(env, &text) }
}

/// A string GL writes into a buffer of `length` bytes, with its terminator.
fn read_string(length: i32, write: impl FnOnce(i32, *mut i32, *mut c_char)) -> String {
    let mut text = vec![0u8; length.clamp(1, 1 << 20) as usize];
    let mut written = 0;
    write(text.len() as i32, &mut written, text.as_mut_ptr().cast());
    text.truncate(written.clamp(0, text.len() as i32) as usize);
    String::from_utf8_lossy(&text).into_owned()
}

type GetIv = unsafe extern "C" fn(u32, u32, *mut i32);
type GetLog = unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char);

macro_rules! host {
    ($env:expr, $name:literal: $type:ty $(, $default:expr)?) => {{
        static ADDRESS: OnceLock<usize> = OnceLock::new();
        // SAFETY: the caller's environment; the host's function has this signature.
        #[allow(unused_unsafe)]
        match unsafe { gl::<$type>($env, &ADDRESS, $name) } {
            Some(function) => function,
            None => return $($default)?,
        }
    }};
}

unsafe extern "system" fn get_string(env: *mut JNIEnv, _class: jclass, name: jint) -> jstring {
    let f = host!(env, c"glGetString": unsafe extern "C" fn(u32) -> *const c_char, std::ptr::null_mut());
    // SAFETY: GL's function and the caller's environment.
    unsafe { new_string(env, f(name as u32)) }
}

unsafe extern "system" fn get_string_i(
    env: *mut JNIEnv,
    _class: jclass,
    name: jint,
    index: jint,
) -> jstring {
    let f = host!(env, c"glGetStringi": unsafe extern "C" fn(u32, u32) -> *const c_char, std::ptr::null_mut());
    // SAFETY: as above.
    unsafe { new_string(env, f(name as u32, index as u32)) }
}

/// A shader's or program's info log or source, whose length GL reports under `length_name`.
///
/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn object_string(env: *mut JNIEnv, object: jint, length: i32, get: GetLog) -> jstring {
    let text = read_string(length, |size, written, text| {
        // SAFETY: GL writes at most `size` bytes into `text`.
        unsafe { get(object as u32, size, written, text) }
    });
    // SAFETY: as above.
    unsafe { crate::new_string(env, &text) }
}

const INFO_LOG_LENGTH: u32 = 0x8B84;
const SHADER_SOURCE_LENGTH: u32 = 0x8B88;

unsafe extern "system" fn shader_info_log(
    env: *mut JNIEnv,
    _class: jclass,
    shader: jint,
) -> jstring {
    let iv = host!(env, c"glGetShaderiv": GetIv, std::ptr::null_mut());
    let log = host!(env, c"glGetShaderInfoLog": GetLog, std::ptr::null_mut());
    let mut length = 0;
    // SAFETY: GL writes one value.
    unsafe { iv(shader as u32, INFO_LOG_LENGTH, &mut length) };
    // SAFETY: as above.
    unsafe { object_string(env, shader, length, log) }
}

unsafe extern "system" fn program_info_log(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
) -> jstring {
    let iv = host!(env, c"glGetProgramiv": GetIv, std::ptr::null_mut());
    let log = host!(env, c"glGetProgramInfoLog": GetLog, std::ptr::null_mut());
    let mut length = 0;
    // SAFETY: GL writes one value.
    unsafe { iv(program as u32, INFO_LOG_LENGTH, &mut length) };
    // SAFETY: as above.
    unsafe { object_string(env, program, length, log) }
}

unsafe extern "system" fn shader_source_string(
    env: *mut JNIEnv,
    _class: jclass,
    shader: jint,
) -> jstring {
    let iv = host!(env, c"glGetShaderiv": GetIv, std::ptr::null_mut());
    let source = host!(env, c"glGetShaderSource": GetLog, std::ptr::null_mut());
    let mut length = 0;
    // SAFETY: GL writes one value.
    unsafe { iv(shader as u32, SHADER_SOURCE_LENGTH, &mut length) };
    // SAFETY: as above.
    unsafe { object_string(env, shader, length, source) }
}

unsafe extern "system" fn shader_source(
    env: *mut JNIEnv,
    _class: jclass,
    shader: jint,
    text: jstring,
) {
    let f = host!(env, c"glShaderSource": unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32));
    // SAFETY: the caller's environment.
    let Some(text) = (unsafe { string(env, text, c"glShaderSource") }) else {
        return;
    };
    let pointer = text.as_ptr();
    // SAFETY: one terminated string; a null length list means each is terminated.
    unsafe { f(shader as u32, 1, &pointer, std::ptr::null()) };
}

type GetActive = unsafe extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char);

/// glGetActiveAttrib, glGetActiveUniform and glGetTransformFeedbackVarying as Java's String forms:
/// the size and type written at `size` and `kind`, the name returned.
///
/// # Safety
/// `env` must be the calling thread's JNI environment; `size` and `kind` hold one value each.
unsafe fn active_name(
    env: *mut JNIEnv,
    program: jint,
    index: jint,
    max_length: u32,
    get: GetActive,
    size: *mut c_void,
    kind: *mut c_void,
) -> jstring {
    let iv = host!(env, c"glGetProgramiv": GetIv, std::ptr::null_mut());
    let mut length = 0;
    // SAFETY: GL writes one value.
    unsafe { iv(program as u32, max_length, &mut length) };
    let text = read_string(length, |bytes, written, name| {
        // SAFETY: GL writes at most `bytes` bytes of name and one value at each other pointer.
        unsafe {
            get(
                program as u32,
                index as u32,
                bytes,
                written,
                size.cast(),
                kind.cast(),
                name,
            )
        }
    });
    // SAFETY: as above.
    unsafe { crate::new_string(env, &text) }
}

macro_rules! active {
    ($array_fn:ident, $buffer_fn:ident, $name:literal, $max:expr) => {
        unsafe extern "system" fn $array_fn(
            env: *mut JNIEnv,
            _class: jclass,
            program: jint,
            index: jint,
            size: jarray,
            size_offset: jint,
            kind: jarray,
            kind_offset: jint,
        ) -> jstring {
            let f = host!(env, $name: GetActive, std::ptr::null_mut());
            // SAFETY: the caller's environment; each array must hold one int.
            unsafe {
                let Some(size) = array(env, size, size_offset, 1, 4, $name) else {
                    return std::ptr::null_mut();
                };
                let Some(kind) = array(env, kind, kind_offset, 1, 4, $name) else {
                    return std::ptr::null_mut();
                };
                // The name is read into memory of ours first, then the arrays are pinned.
                let iv = host!(env, c"glGetProgramiv": GetIv, std::ptr::null_mut());
                let mut length = 0;
                iv(program as u32, $max, &mut length);
                let (mut size_value, mut kind_value) = (0i32, 0i32);
                let text = read_string(length, |bytes, written, name| {
                    f(program as u32, index as u32, bytes, written, &mut size_value, (&mut kind_value as *mut i32).cast(), name)
                });
                *size.pointer().cast::<i32>() = size_value;
                *kind.pointer().cast::<i32>() = kind_value;
                drop((size, kind));
                crate::new_string(env, &text)
            }
        }

        unsafe extern "system" fn $buffer_fn(
            env: *mut JNIEnv,
            _class: jclass,
            program: jint,
            index: jint,
            size: jobject,
            kind: jobject,
        ) -> jstring {
            let f = host!(env, $name: GetActive, std::ptr::null_mut());
            // SAFETY: the caller's environment; each buffer must hold one int.
            unsafe {
                let Some(size) = buffer(env, size, 4, false, $name) else {
                    return std::ptr::null_mut();
                };
                let Some(kind) = buffer(env, kind, 4, false, $name) else {
                    return std::ptr::null_mut();
                };
                let (mut size_value, mut kind_value) = (0i32, 0i32);
                let text = active_name(
                    env,
                    program,
                    index,
                    $max,
                    f,
                    (&mut size_value as *mut i32).cast(),
                    (&mut kind_value as *mut i32).cast(),
                );
                *size.pointer().cast::<i32>() = size_value;
                *kind.pointer().cast::<i32>() = kind_value;
                text
            }
        }
    };
}

active!(
    active_attrib_array,
    active_attrib_buffer,
    c"glGetActiveAttrib",
    0x8B8A
);
active!(
    active_uniform_array,
    active_uniform_buffer,
    c"glGetActiveUniform",
    0x8B87
);
active!(
    feedback_varying_array,
    feedback_varying_buffer,
    c"glGetTransformFeedbackVarying",
    0x8C76
);

unsafe extern "system" fn uniform_block_name(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
    index: jint,
) -> jstring {
    let iv = host!(env, c"glGetActiveUniformBlockiv": unsafe extern "C" fn(u32, u32, u32, *mut i32), std::ptr::null_mut());
    let name = host!(env, c"glGetActiveUniformBlockName": unsafe extern "C" fn(u32, u32, i32, *mut i32, *mut c_char), std::ptr::null_mut());
    let mut length = 0;
    // SAFETY: GL writes one value for GL_UNIFORM_BLOCK_NAME_LENGTH.
    unsafe { iv(program as u32, index as u32, 0x8A41, &mut length) };
    let text = read_string(length, |bytes, written, text| {
        // SAFETY: GL writes at most `bytes` bytes.
        unsafe { name(program as u32, index as u32, bytes, written, text) }
    });
    // SAFETY: the caller's environment.
    unsafe { crate::new_string(env, &text) }
}

/// The strings of a Java String[], or None with IllegalArgumentException thrown.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `strings` a String[] or null.
unsafe fn strings(
    env: *mut JNIEnv,
    strings: jobjectArray,
    function: &CStr,
) -> Option<Vec<std::ffi::CString>> {
    if strings.is_null() {
        // SAFETY: as above.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalArgumentException",
                &format!("{}: names == null", function.to_string_lossy()),
            )
        };
        return None;
    }
    // SAFETY: as above.
    let length = unsafe { ((**env).v1_1.GetArrayLength)(env, strings) };
    let mut found = Vec::with_capacity(length.max(0) as usize);
    for index in 0..length {
        // SAFETY: as above, within the array's length.
        let item = unsafe { ((**env).v1_1.GetObjectArrayElement)(env, strings, index) };
        // SAFETY: as above.
        found.push(unsafe { string(env, item, function) }?);
        // SAFETY: a local reference of this call.
        unsafe { ((**env).v1_1.DeleteLocalRef)(env, item) };
    }
    Some(found)
}

unsafe extern "system" fn feedback_varyings(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
    names: jobjectArray,
    mode: jint,
) {
    let f = host!(env, c"glTransformFeedbackVaryings": unsafe extern "C" fn(u32, i32, *const *const c_char, u32));
    // SAFETY: the caller's environment.
    let Some(names) = (unsafe { strings(env, names, c"glTransformFeedbackVaryings") }) else {
        return;
    };
    let pointers: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
    // SAFETY: GL reads `pointers.len()` terminated strings.
    unsafe {
        f(
            program as u32,
            pointers.len() as i32,
            pointers.as_ptr(),
            mode as u32,
        )
    };
}

type GetIndices = unsafe extern "C" fn(u32, i32, *const *const c_char, *mut u32);

unsafe extern "system" fn uniform_indices_array(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
    names: jobjectArray,
    indices: jarray,
    offset: jint,
) {
    let f = host!(env, c"glGetUniformIndices": GetIndices);
    // SAFETY: the caller's environment; GL writes one index per name.
    unsafe {
        let Some(names) = strings(env, names, c"glGetUniformIndices") else {
            return;
        };
        let Some(indices) = array(env, indices, offset, names.len(), 4, c"glGetUniformIndices")
        else {
            return;
        };
        let pointers: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
        f(
            program as u32,
            pointers.len() as i32,
            pointers.as_ptr(),
            indices.pointer().cast(),
        );
    }
}

unsafe extern "system" fn uniform_indices_buffer(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
    names: jobjectArray,
    indices: jobject,
) {
    let f = host!(env, c"glGetUniformIndices": GetIndices);
    // SAFETY: as above.
    unsafe {
        let Some(names) = strings(env, names, c"glGetUniformIndices") else {
            return;
        };
        let Some(indices) = buffer(env, indices, names.len() * 4, false, c"glGetUniformIndices")
        else {
            return;
        };
        let pointers: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
        f(
            program as u32,
            pointers.len() as i32,
            pointers.as_ptr(),
            indices.pointer().cast(),
        );
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `address` hold `length` bytes or be null.
unsafe fn direct_buffer(env: *mut JNIEnv, address: *mut c_void, length: i64) -> jobject {
    if address.is_null() || length < 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: as above.
    unsafe { ((**env).v1_4.NewDirectByteBuffer)(env, address, length) }
}

unsafe extern "system" fn map_buffer_range(
    env: *mut JNIEnv,
    _class: jclass,
    target: jint,
    offset: jint,
    length: jint,
    access: jint,
) -> jobject {
    let f = host!(env, c"glMapBufferRange": unsafe extern "C" fn(u32, isize, isize, u32) -> *mut c_void, std::ptr::null_mut());
    // SAFETY: GL maps `length` bytes, which the buffer covers until it is unmapped.
    unsafe {
        direct_buffer(
            env,
            f(
                target as u32,
                offset as isize,
                length as isize,
                access as u32,
            ),
            length as i64,
        )
    }
}

unsafe extern "system" fn buffer_pointer(
    env: *mut JNIEnv,
    _class: jclass,
    target: jint,
    name: jint,
) -> jobject {
    let pointer = host!(env, c"glGetBufferPointerv": unsafe extern "C" fn(u32, u32, *mut *mut c_void), std::ptr::null_mut());
    let iv = host!(env, c"glGetBufferParameteri64v": unsafe extern "C" fn(u32, u32, *mut i64), std::ptr::null_mut());
    let (mut address, mut length) = (std::ptr::null_mut(), 0i64);
    // SAFETY: GL writes one value each; GL_BUFFER_MAP_LENGTH is the mapped length.
    unsafe {
        pointer(target as u32, name as u32, &mut address);
        iv(target as u32, 0x9120, &mut length);
        direct_buffer(env, address, length)
    }
}

/// glVertexAttribPointer with client memory, which GL reads at each draw: only a direct buffer stays
/// put, as AOSP's binding requires.
unsafe extern "system" fn vertex_attrib_pointer_bounds(
    env: *mut JNIEnv,
    _class: jclass,
    index: jint,
    size: jint,
    kind: jint,
    normalized: jni_sys::jboolean,
    stride: jint,
    pointer: jobject,
    _remaining: jint,
) {
    let f = host!(env, c"glVertexAttribPointer": unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void));
    if pointer.is_null() {
        // SAFETY: the caller's environment.
        unsafe {
            f(
                index as u32,
                size,
                kind as u32,
                normalized as u8,
                stride,
                std::ptr::null(),
            )
        };
        return;
    }
    // SAFETY: as above.
    let address = unsafe { ((**env).v1_4.GetDirectBufferAddress)(env, pointer) };
    if address.is_null() {
        // SAFETY: as above.
        unsafe {
            throw(
                env,
                c"java/lang/IllegalArgumentException",
                "Must use a native order direct Buffer",
            )
        };
        return;
    }
    // SAFETY: as above; the position is the buffer's own, within it.
    unsafe {
        let Some(pinned) = buffer(env, pointer, 0, false, c"glVertexAttribPointer") else {
            return;
        };
        f(
            index as u32,
            size,
            kind as u32,
            normalized as u8,
            stride,
            pinned.pointer(),
        );
    }
}

/// glVertexAttribIPointer with client memory: a direct buffer only, as above.
unsafe extern "system" fn vertex_attrib_i_pointer_bounds(
    env: *mut JNIEnv,
    _class: jclass,
    index: jint,
    size: jint,
    kind: jint,
    stride: jint,
    pointer: jobject,
    _remaining: jint,
) {
    let f = host!(env, c"glVertexAttribIPointer": unsafe extern "C" fn(u32, i32, u32, i32, *const c_void));
    // SAFETY: the caller's environment.
    unsafe {
        if !pointer.is_null() && ((**env).v1_4.GetDirectBufferAddress)(env, pointer).is_null() {
            throw(
                env,
                c"java/lang/IllegalArgumentException",
                "Must use a native order direct Buffer",
            );
            return;
        }
        let Some(pinned) = buffer(env, pointer, 0, true, c"glVertexAttribIPointer") else {
            return;
        };
        f(index as u32, size, kind as u32, stride, pinned.pointer());
    }
}

/// The buffer form of glGetActiveUniformBlockName, whose name buffer's remaining bytes are the size.
unsafe extern "system" fn uniform_block_name_buffer(
    env: *mut JNIEnv,
    _class: jclass,
    program: jint,
    index: jint,
    length: jobject,
    name: jobject,
) {
    let f = host!(env, c"glGetActiveUniformBlockName": unsafe extern "C" fn(u32, u32, i32, *mut i32, *mut c_char));
    // SAFETY: the caller's environment; GL writes one length and at most the name's room.
    unsafe {
        let Some(length) = buffer(env, length, 4, false, c"glGetActiveUniformBlockName") else {
            return;
        };
        let Some(name) = buffer(env, name, 1, false, c"glGetActiveUniformBlockName") else {
            return;
        };
        let room = name.remaining.min(i32::MAX as usize) as i32;
        f(
            program as u32,
            index as u32,
            room,
            length.pointer().cast(),
            name.pointer().cast(),
        );
    }
}

const HAND_GLES20: &[Method] = &[
    (c"_nativeClassInit", c"()V", class_init as *mut c_void),
    (
        c"glGetString",
        c"(I)Ljava/lang/String;",
        get_string as *mut c_void,
    ),
    (
        c"glGetShaderInfoLog",
        c"(I)Ljava/lang/String;",
        shader_info_log as *mut c_void,
    ),
    (
        c"glGetProgramInfoLog",
        c"(I)Ljava/lang/String;",
        program_info_log as *mut c_void,
    ),
    (
        c"glGetShaderSource",
        c"(I)Ljava/lang/String;",
        shader_source_string as *mut c_void,
    ),
    (
        c"glShaderSource",
        c"(ILjava/lang/String;)V",
        shader_source as *mut c_void,
    ),
    (
        c"glGetActiveAttrib",
        c"(II[II[II)Ljava/lang/String;",
        active_attrib_array as *mut c_void,
    ),
    (
        c"glGetActiveAttrib",
        c"(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;",
        active_attrib_buffer as *mut c_void,
    ),
    (
        c"glGetActiveUniform",
        c"(II[II[II)Ljava/lang/String;",
        active_uniform_array as *mut c_void,
    ),
    (
        c"glGetActiveUniform",
        c"(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;",
        active_uniform_buffer as *mut c_void,
    ),
    (
        c"glVertexAttribPointerBounds",
        c"(IIIZILjava/nio/Buffer;I)V",
        vertex_attrib_pointer_bounds as *mut c_void,
    ),
];

/// GLImpl's own names for functions GLES has.
const HAND_GL_IMPL: &[Method] = &[(
    c"_glGetString",
    c"(I)Ljava/lang/String;",
    get_string as *mut c_void,
)];

const HAND_GLES30: &[Method] = &[
    (c"_nativeClassInit", c"()V", class_init as *mut c_void),
    (
        c"glGetStringi",
        c"(II)Ljava/lang/String;",
        get_string_i as *mut c_void,
    ),
    (
        c"glGetTransformFeedbackVarying",
        c"(II[II[II)Ljava/lang/String;",
        feedback_varying_array as *mut c_void,
    ),
    (
        c"glGetTransformFeedbackVarying",
        c"(IILjava/nio/IntBuffer;Ljava/nio/IntBuffer;)Ljava/lang/String;",
        feedback_varying_buffer as *mut c_void,
    ),
    (
        c"glGetActiveUniformBlockName",
        c"(II)Ljava/lang/String;",
        uniform_block_name as *mut c_void,
    ),
    (
        c"glTransformFeedbackVaryings",
        c"(I[Ljava/lang/String;I)V",
        feedback_varyings as *mut c_void,
    ),
    (
        c"glGetUniformIndices",
        c"(I[Ljava/lang/String;[II)V",
        uniform_indices_array as *mut c_void,
    ),
    (
        c"glGetUniformIndices",
        c"(I[Ljava/lang/String;Ljava/nio/IntBuffer;)V",
        uniform_indices_buffer as *mut c_void,
    ),
    (
        c"glMapBufferRange",
        c"(IIII)Ljava/nio/Buffer;",
        map_buffer_range as *mut c_void,
    ),
    (
        c"glGetBufferPointerv",
        c"(II)Ljava/nio/Buffer;",
        buffer_pointer as *mut c_void,
    ),
    (
        c"glVertexAttribIPointerBounds",
        c"(IIIILjava/nio/Buffer;I)V",
        vertex_attrib_i_pointer_bounds as *mut c_void,
    ),
    (
        c"glGetActiveUniformBlockName",
        c"(IILjava/nio/Buffer;Ljava/nio/Buffer;)V",
        uniform_block_name_buffer as *mut c_void,
    ),
];
