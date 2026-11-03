//! `libEGL` and the GLES libraries over the host's Mesa.
//!
//! GLES has the same C ABI on both sides, so its table binds Mesa's own functions, at no cost. EGL
//! differs where Android's window system shows through, and those functions are translated here:
//!
//! - the display is Mesa's surfaceless platform, which needs no window system and renders on the
//!   GPU's render node, or with llvmpipe where there is none, as in the test container;
//! - a window surface on an `ANativeWindow` is a pbuffer of the window's size, and swapping reads
//!   it back and posts it through the window, which shows it in the Activity's Wayland window;
//! - configs report `EGL_WINDOW_BIT` where they support pbuffers, and the visual ID Android gives,
//!   which is the `ANativeWindow` format of their color sizes.
//!
//! The readback costs a copy per frame; an EGL window on the Activity's own `wl_surface` would remove
//! it once the window system shares that surface. Every other EGL function goes straight to Mesa.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use crate::java::{Jni, Thrown};
use crate::window::{self, Window};

type Display = *mut c_void;
type Config = *mut c_void;
type Surface = *mut c_void;
type Boolean = u32;

const FALSE: Boolean = 0;
const TRUE: Boolean = 1;
const NONE: i32 = 0x3038;
const DONT_CARE: i32 = -1;
const SURFACE_TYPE: i32 = 0x3033;
const PBUFFER_BIT: i32 = 0x0001;
const WINDOW_BIT: i32 = 0x0004;
const NATIVE_VISUAL_ID: i32 = 0x302E;
const RED_SIZE: i32 = 0x3024;
const GREEN_SIZE: i32 = 0x3023;
const BLUE_SIZE: i32 = 0x3022;
const ALPHA_SIZE: i32 = 0x3021;
const WIDTH: i32 = 0x3057;
const HEIGHT: i32 = 0x3056;
const DRAW: i32 = 0x3059;
const CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const GL_COLORSPACE: i32 = 0x309D;
const RECORDABLE_ANDROID: i32 = 0x3142;
const FRAMEBUFFER_TARGET_ANDROID: i32 = 0x3147;
const PLATFORM_ANDROID: u32 = 0x3141;
const PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
/// Bounds the attribute lists read from app code.
const MAX_ATTRIBUTES: usize = 256;

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
const GL_PACK_ROW_LENGTH: u32 = 0x0D02;
const GL_PACK_SKIP_ROWS: u32 = 0x0D03;
const GL_PACK_SKIP_PIXELS: u32 = 0x0D04;
const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
const GL_PIXEL_PACK_BUFFER_BINDING: u32 = 0x88ED;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;

fn open(name: &CStr) -> usize {
    // SAFETY: a C string; a missing library leaves the handle null, and every lookup then fails.
    unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) as usize }
}

fn libegl() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| open(c"libEGL.so.1"))
}

fn libgles() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| open(c"libGLESv2.so.2"))
}

fn lookup(handle: usize, name: &CStr) -> Option<*const c_void> {
    if handle == 0 {
        return None;
    }
    // SAFETY: a handle from dlopen and a C string.
    let address = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
    (!address.is_null()).then_some(address.cast_const())
}

/// Mesa's `eglGetProcAddress`, which also finds extensions the libraries do not export.
fn proc_address(name: &CStr) -> Option<*const c_void> {
    type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;
    let get = lookup(libegl(), c"eglGetProcAddress")?;
    // SAFETY: Mesa's eglGetProcAddress has this signature, and `name` is a C string.
    let address =
        unsafe { std::mem::transmute::<*const c_void, GetProcAddress>(get)(name.as_ptr()) };
    (!address.is_null()).then_some(address)
}

/// Binds an EGL export the table passes straight to Mesa.
pub fn egl_symbol(name: &CStr) -> Option<*const c_void> {
    lookup(libegl(), name).or_else(|| proc_address(name))
}

/// Binds a GLES export to Mesa.
pub fn gles_symbol(name: &CStr) -> Option<*const c_void> {
    lookup(libgles(), name).or_else(|| proc_address(name))
}

macro_rules! mesa {
    ($($field:ident: $name:literal => fn($($argument:ty),*) $(-> $returns:ty)?;)*) => {
        /// The Mesa functions the translated ones call.
        struct Mesa {
            $($field: unsafe extern "C" fn($($argument),*) $(-> $returns)?,)*
        }

        fn mesa() -> Option<&'static Mesa> {
            static MESA: OnceLock<Option<Mesa>> = OnceLock::new();
            MESA.get_or_init(|| {
                // SAFETY: each name is looked up in Mesa's libraries, where it has this signature.
                Some(Mesa {
                    $($field: unsafe {
                        std::mem::transmute::<*const c_void, unsafe extern "C" fn($($argument),*) $(-> $returns)?>(gles_or_egl($name)?)
                    },)*
                })
            })
            .as_ref()
        }
    };
}

fn gles_or_egl(name: &CStr) -> Option<*const c_void> {
    if name.to_bytes().starts_with(b"gl") {
        gles_symbol(name)
    } else {
        egl_symbol(name)
    }
}

mesa! {
    get_platform_display: c"eglGetPlatformDisplay" => fn(u32, *mut c_void, *const isize) -> Display;
    choose_config: c"eglChooseConfig" => fn(Display, *const i32, *mut Config, i32, *mut i32) -> Boolean;
    get_config_attrib: c"eglGetConfigAttrib" => fn(Display, Config, i32, *mut i32) -> Boolean;
    create_pbuffer: c"eglCreatePbufferSurface" => fn(Display, Config, *const i32) -> Surface;
    destroy_surface: c"eglDestroySurface" => fn(Display, Surface) -> Boolean;
    swap_buffers: c"eglSwapBuffers" => fn(Display, Surface) -> Boolean;
    get_current_surface: c"eglGetCurrentSurface" => fn(i32) -> Surface;
    get_current_context: c"eglGetCurrentContext" => fn() -> *mut c_void;
    query_context: c"eglQueryContext" => fn(Display, *mut c_void, i32, *mut i32) -> Boolean;
    query_surface: c"eglQuerySurface" => fn(Display, Surface, i32, *mut i32) -> Boolean;
    get_integer: c"glGetIntegerv" => fn(u32, *mut i32);
    bind_framebuffer: c"glBindFramebuffer" => fn(u32, u32);
    bind_buffer: c"glBindBuffer" => fn(u32, u32);
    pixel_store: c"glPixelStorei" => fn(u32, i32);
    read_pixels: c"glReadPixels" => fn(i32, i32, i32, i32, u32, u32, *mut c_void);
}

/// Window surfaces by their pbuffer, each holding a reference to its window.
static WINDOW_SURFACES: Mutex<Option<HashMap<usize, Arc<Window>>>> = Mutex::new(None);

fn window_surfaces() -> std::sync::MutexGuard<'static, Option<HashMap<usize, Arc<Window>>>> {
    WINDOW_SURFACES.lock().unwrap_or_else(|p| p.into_inner())
}

/// Reads an attribute list up to `EGL_NONE`, or `None` when it has no end within the bound.
///
/// # Safety
/// `list` must be null or a list of pairs ended by `EGL_NONE`, as EGL requires.
unsafe fn attributes<T: Copy + PartialEq>(list: *const T, none: T) -> Option<Vec<(T, T)>> {
    let mut pairs = Vec::new();
    if list.is_null() {
        return Some(pairs);
    }
    for index in 0..MAX_ATTRIBUTES {
        // SAFETY: as above; reading stops at EGL_NONE, so no pair past the end is read.
        let key = unsafe { *list.add(index * 2) };
        if key == none {
            return Some(pairs);
        }
        // SAFETY: as above.
        pairs.push((key, unsafe { *list.add(index * 2 + 1) }));
    }
    None
}

fn surfaceless() -> Display {
    static DISPLAY: OnceLock<usize> = OnceLock::new();
    *DISPLAY.get_or_init(|| {
        let Some(mesa) = mesa() else {
            eprintln!("uxdroid: EGL needs the host's Mesa (libEGL.so.1, libGLESv2.so.2)");
            return 0;
        };
        // SAFETY: Mesa's function; the surfaceless platform takes no native display.
        unsafe {
            (mesa.get_platform_display)(
                PLATFORM_SURFACELESS_MESA,
                std::ptr::null_mut(),
                std::ptr::null(),
            ) as usize
        }
    }) as Display
}

/// `EGLDisplay eglGetDisplay(EGLNativeDisplayType)`: Android has one display, the default.
pub unsafe extern "C" fn get_display(_native: *mut c_void) -> Display {
    surfaceless()
}

/// `EGLDisplay eglGetPlatformDisplay(EGLenum, void*, const EGLAttrib*)`.
pub unsafe extern "C" fn get_platform_display(
    platform: u32,
    native: *mut c_void,
    list: *const isize,
) -> Display {
    if platform == PLATFORM_ANDROID {
        return surfaceless();
    }
    match mesa() {
        // SAFETY: forwards the caller's arguments to Mesa under the same contract.
        Some(mesa) => unsafe { (mesa.get_platform_display)(platform, native, list) },
        None => std::ptr::null_mut(),
    }
}

/// `EGLBoolean eglChooseConfig(...)`: a window, which EGL asks for by default, is a pbuffer here.
pub unsafe extern "C" fn choose_config(
    display: Display,
    list: *const i32,
    configs: *mut Config,
    size: i32,
    count: *mut i32,
) -> Boolean {
    let Some(mesa) = mesa() else {
        return FALSE;
    };
    // SAFETY: EGL's contract for attribute lists.
    let Some(pairs) = (unsafe { attributes(list, NONE) }) else {
        return FALSE;
    };
    let mut translated = Vec::with_capacity(pairs.len() * 2 + 3);
    let mut surface_type = false;
    for (key, value) in pairs {
        match key {
            RECORDABLE_ANDROID | FRAMEBUFFER_TARGET_ANDROID => continue,
            SURFACE_TYPE => {
                surface_type = true;
                let value = if value != DONT_CARE && value & WINDOW_BIT != 0 {
                    (value & !WINDOW_BIT) | PBUFFER_BIT
                } else {
                    value
                };
                translated.extend([key, value]);
            }
            _ => translated.extend([key, value]),
        }
    }
    if !surface_type {
        translated.extend([SURFACE_TYPE, PBUFFER_BIT]);
    }
    translated.push(NONE);
    // SAFETY: forwards to Mesa with a terminated list and the caller's output arguments.
    unsafe { (mesa.choose_config)(display, translated.as_ptr(), configs, size, count) }
}

/// `EGLBoolean eglGetConfigAttrib(...)`.
pub unsafe extern "C" fn get_config_attrib(
    display: Display,
    config: Config,
    attribute: i32,
    value: *mut i32,
) -> Boolean {
    let Some(mesa) = mesa() else {
        return FALSE;
    };
    if value.is_null() {
        return FALSE;
    }
    let size = |attribute| {
        let mut size = 0;
        // SAFETY: forwards to Mesa with a local output.
        (unsafe { (mesa.get_config_attrib)(display, config, attribute, &mut size) } == TRUE)
            .then_some(size)
    };
    let answer = match attribute {
        NATIVE_VISUAL_ID => {
            let sizes = [RED_SIZE, GREEN_SIZE, BLUE_SIZE, ALPHA_SIZE].map(size);
            let [Some(red), Some(green), Some(blue), Some(alpha)] = sizes else {
                return FALSE;
            };
            // HAL_PIXEL_FORMAT_*, the ANativeWindow formats.
            match (red, green, blue, alpha) {
                (8, 8, 8, 8) => 1,
                (8, 8, 8, 0) => 2,
                (5, 6, 5, 0) => 4,
                _ => 0,
            }
        }
        SURFACE_TYPE => {
            let Some(kind) = size(SURFACE_TYPE) else {
                return FALSE;
            };
            if kind & PBUFFER_BIT != 0 {
                kind | WINDOW_BIT
            } else {
                kind
            }
        }
        // SAFETY: forwards the caller's arguments to Mesa.
        _ => return unsafe { (mesa.get_config_attrib)(display, config, attribute, value) },
    };
    // SAFETY: checked non-null above; EGL's contract makes it writable.
    unsafe { *value = answer };
    TRUE
}

/// `EGLSurface eglCreateWindowSurface(EGLDisplay, EGLConfig, EGLNativeWindowType, const EGLint*)`.
pub unsafe extern "C" fn create_window_surface(
    display: Display,
    config: Config,
    native: *mut c_void,
    list: *const i32,
) -> Surface {
    // SAFETY: EGL's contract for attribute lists.
    match unsafe { attributes(list, NONE) } {
        Some(pairs) => window_surface(display, config, native, &pairs),
        None => std::ptr::null_mut(),
    }
}

/// `EGLSurface eglCreatePlatformWindowSurface(..., void* native_window, const EGLAttrib*)`: on
/// Android, the native window is the `ANativeWindow*` itself.
pub unsafe extern "C" fn create_platform_window_surface(
    display: Display,
    config: Config,
    native: *mut c_void,
    list: *const isize,
) -> Surface {
    // SAFETY: EGL's contract for attribute lists.
    let Some(pairs) = (unsafe { attributes(list, NONE as isize) }) else {
        return std::ptr::null_mut();
    };
    let pairs: Option<Vec<(i32, i32)>> = pairs
        .into_iter()
        .map(|(key, value)| Some((i32::try_from(key).ok()?, i32::try_from(value).ok()?)))
        .collect();
    pairs.map_or(std::ptr::null_mut(), |pairs| {
        window_surface(display, config, native, &pairs)
    })
}

fn window_surface(
    display: Display,
    config: Config,
    native: *mut c_void,
    pairs: &[(i32, i32)],
) -> Surface {
    let (Some(mesa), Some(window), Some(jni)) = (mesa(), window::get(native), Jni::current())
    else {
        return std::ptr::null_mut();
    };
    let Ok((width, height, _)) = window.buffer_size(jni) else {
        return std::ptr::null_mut();
    };
    let mut list = vec![WIDTH, width, HEIGHT, height];
    // A window's other attributes, such as its render buffer, do not apply to a pbuffer.
    for &(key, value) in pairs {
        if key == GL_COLORSPACE {
            list.extend([key, value]);
        }
    }
    list.push(NONE);
    // SAFETY: Mesa's function with a terminated list.
    let surface = unsafe { (mesa.create_pbuffer)(display, config, list.as_ptr()) };
    if !surface.is_null() {
        // SAFETY: a live window of this module; the reference goes back in destroy_surface.
        unsafe { window::acquire(native) };
        window_surfaces()
            .get_or_insert_with(HashMap::new)
            .insert(surface as usize, window);
    }
    surface
}

/// `EGLBoolean eglDestroySurface(EGLDisplay, EGLSurface)`.
pub unsafe extern "C" fn destroy_surface(display: Display, surface: Surface) -> Boolean {
    let Some(mesa) = mesa() else {
        return FALSE;
    };
    let window = window_surfaces()
        .get_or_insert_with(HashMap::new)
        .remove(&(surface as usize));
    if let Some(window) = window {
        // SAFETY: gives back the reference window_surface took.
        unsafe { window::release(Arc::as_ptr(&window).cast_mut().cast()) };
    }
    // SAFETY: forwards the caller's arguments to Mesa.
    unsafe { (mesa.destroy_surface)(display, surface) }
}

/// Copies a window surface's pixels into its window and posts them.
fn present(mesa: &Mesa, display: Display, surface: Surface, window: &Window) -> Result<(), Thrown> {
    let jni = Jni::current().ok_or(Thrown)?;
    // SAFETY: Mesa's functions on the calling thread's current context.
    unsafe {
        if (mesa.get_current_surface)(DRAW) != surface {
            return Err(Thrown);
        }
        let (mut width, mut height) = (0, 0);
        (mesa.query_surface)(display, surface, WIDTH, &mut width);
        (mesa.query_surface)(display, surface, HEIGHT, &mut height);
        let mut version = 2;
        (mesa.query_context)(
            display,
            (mesa.get_current_context)(),
            CONTEXT_CLIENT_VERSION,
            &mut version,
        );
        let (bits, width, height, _) = window.lock_exact(jni, width, height)?;

        let mut framebuffer = 0;
        (mesa.get_integer)(GL_FRAMEBUFFER_BINDING, &mut framebuffer);
        let mut alignment = 4;
        (mesa.get_integer)(GL_PACK_ALIGNMENT, &mut alignment);
        let es3_state = [
            GL_PIXEL_PACK_BUFFER_BINDING,
            GL_PACK_ROW_LENGTH,
            GL_PACK_SKIP_ROWS,
            GL_PACK_SKIP_PIXELS,
        ];
        let mut saved = [0; 4];
        if version >= 3 {
            for (name, value) in es3_state.iter().zip(&mut saved) {
                (mesa.get_integer)(*name, value);
            }
            (mesa.bind_buffer)(GL_PIXEL_PACK_BUFFER, 0);
            for name in &es3_state[1..] {
                (mesa.pixel_store)(*name, 0);
            }
        }
        (mesa.bind_framebuffer)(GL_FRAMEBUFFER, 0);
        (mesa.pixel_store)(GL_PACK_ALIGNMENT, 4);
        // The buffer holds width * height RGBA pixels, which the pack state above makes tight.
        (mesa.read_pixels)(0, 0, width, height, GL_RGBA, GL_UNSIGNED_BYTE, bits.cast());
        (mesa.pixel_store)(GL_PACK_ALIGNMENT, alignment);
        (mesa.bind_framebuffer)(GL_FRAMEBUFFER, framebuffer as u32);
        if version >= 3 {
            (mesa.bind_buffer)(GL_PIXEL_PACK_BUFFER, saved[0] as u32);
            for (name, value) in es3_state[1..].iter().zip(&saved[1..]) {
                (mesa.pixel_store)(*name, *value);
            }
        }
        // GL rows go up from the bottom, a window's down from the top.
        let row = width as usize * 4;
        let pixels = std::slice::from_raw_parts_mut(bits, row * height as usize);
        for top in 0..height as usize / 2 {
            let bottom = height as usize - 1 - top;
            let (upper, lower) = pixels.split_at_mut(bottom * row);
            upper[top * row..(top + 1) * row].swap_with_slice(&mut lower[..row]);
        }
    }
    window.post(jni)
}

/// `EGLBoolean eglSwapBuffers(EGLDisplay, EGLSurface)`.
pub unsafe extern "C" fn swap_buffers(display: Display, surface: Surface) -> Boolean {
    let Some(mesa) = mesa() else {
        return FALSE;
    };
    let window = window_surfaces()
        .get_or_insert_with(HashMap::new)
        .get(&(surface as usize))
        .cloned();
    if let Some(window) = window
        && present(mesa, display, surface, &window).is_err()
    {
        eprintln!(
            "uxdroid: eglSwapBuffers could not post a frame of a surface that is not current"
        );
    }
    // SAFETY: forwards the caller's arguments to Mesa.
    unsafe { (mesa.swap_buffers)(display, surface) }
}

/// `EGLBoolean eglSwapBuffersWithDamageKHR(..., const EGLint* rects, EGLint n_rects)`: every frame
/// posts whole, so the damage changes nothing.
pub unsafe extern "C" fn swap_buffers_with_damage(
    display: Display,
    surface: Surface,
    _rects: *const i32,
    _count: i32,
) -> Boolean {
    // SAFETY: forwards the caller's arguments.
    unsafe { swap_buffers(display, surface) }
}

/// `EGLBoolean eglPresentationTimeANDROID(EGLDisplay, EGLSurface, EGLnsecsANDROID)`: frames post
/// at once, so a requested presentation time is accepted and has no effect.
pub unsafe extern "C" fn presentation_time(
    _display: Display,
    _surface: Surface,
    _time: i64,
) -> Boolean {
    TRUE
}

/// `__eglMustCastToProperFunctionPointerType eglGetProcAddress(const char*)`: the translated
/// functions for their names, Mesa's for the rest.
pub unsafe extern "C" fn get_proc_address(name: *const c_char) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: EGL's contract makes `name` a C string.
    let name = unsafe { CStr::from_ptr(name) };
    let own = crate::egl_table::EGL
        .exports
        .iter()
        .find(|export| export.name == name)
        .and_then(|export| match export.address {
            uxdroid_bridge::table::Address::Function(address) => Some(address.cast::<c_void>()),
            _ => None,
        });
    own.or_else(|| proc_address(name))
        .unwrap_or(std::ptr::null())
}
