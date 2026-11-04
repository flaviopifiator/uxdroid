//! EGL window surfaces on the GPU, with frames handed to the compositor as dmabufs.
//!
//! Only vendor-neutral interfaces are used: GBM opens the render node and allocates buffers, EGL's
//! GBM platform gives the display, `EGL_EXT_image_dma_buf_import` makes each buffer a renderbuffer,
//! and `EGL_KHR_fence_sync` waits for the GPU before the compositor reads. The app still draws into
//! a pbuffer, its framebuffer 0, as on every other path; a swap blits that framebuffer into the next
//! free buffer of a swapchain of two or three, upside down, since GL's rows go up and a dmabuf's go
//! down, and hands the dmabuf to the window system (`crate::present`). No pixel crosses the CPU.
//!
//! GBM window surfaces would save the blit, but the host's NVIDIA driver refuses them on a render
//! node (`EGL_BAD_ALLOC`, `runtime/gpu/dmabuf_probe.c`), so the blit is the path every driver takes.
//! Without a render node, GBM, the extensions or a compositor that imports dmabufs, frames are read
//! back as before.

use std::ffi::{CStr, CString, c_void};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::present::{Frame, Presenter, SHOWN};

/// `DRM_FORMAT_ARGB8888`: every buffer has alpha; the window system shows it as XRGB8888 when the
/// layer is opaque.
const ARGB8888: u32 = 0x3432_5241;
const INVALID_MODIFIER: u64 = 0x00ff_ffff_ffff_ffff;
const GBM_BO_USE_RENDERING: u32 = 1 << 2;

const PLATFORM_GBM: u32 = 0x31D7;
const EXTENSIONS: i32 = 0x3055;
const VENDOR: i32 = 0x3053;
const NONE: i32 = 0x3038;
const SURFACE_TYPE: i32 = 0x3033;
const PBUFFER_BIT: i32 = 0x0001;
const RENDERABLE_TYPE: i32 = 0x3040;
const OPENGL_ES2_BIT: i32 = 0x0004;
const WIDTH: i32 = 0x3057;
const HEIGHT: i32 = 0x3056;
const LINUX_DMA_BUF: u32 = 0x3270;
const LINUX_DRM_FOURCC: i32 = 0x3271;
const PLANE0_FD: i32 = 0x3272;
const PLANE0_OFFSET: i32 = 0x3273;
const PLANE0_PITCH: i32 = 0x3274;
const PLANE0_MODIFIER_LO: i32 = 0x3443;
const PLANE0_MODIFIER_HI: i32 = 0x3444;
const SYNC_FENCE: u32 = 0x30F9;
const SYNC_FLUSH_COMMANDS_BIT: i32 = 0x0001;
const CONDITION_SATISFIED: i32 = 0x30F6;

const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_RENDERBUFFER: u32 = 0x8D41;
const GL_RENDERBUFFER_BINDING: u32 = 0x8CA7;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_NEAREST: u32 = 0x2600;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_RASTERIZER_DISCARD: u32 = 0x8C89;
const GL_SAMPLE_BUFFERS: u32 = 0x80A8;
const GL_VERSION: u32 = 0x1F02;

/// Buffers a swapchain grows to while the compositor holds the others.
const MAX_SLOTS: usize = 3;
/// How long a swap waits for a buffer to come back before reading the frame back instead.
const WAIT_FOR_BUFFER: Duration = Duration::from_millis(50);

/// Which buffer a swap draws into.
#[derive(Debug, PartialEq)]
enum Choice {
    Use(usize),
    Grow,
    Wait,
}

/// The first free buffer, else a new one while there are fewer than `max`, else wait.
fn choose(busy: &[bool], max: usize) -> Choice {
    match busy.iter().position(|busy| !busy) {
        Some(at) => Choice::Use(at),
        None if busy.len() < max => Choice::Grow,
        None => Choice::Wait,
    }
}

/// The modifiers to allocate with: the compositor's list without the implicit one, which GBM's
/// plain allocation stands for. Empty means a plain allocation.
fn allocation_modifiers(listed: &[u64]) -> Vec<u64> {
    listed
        .iter()
        .copied()
        .filter(|&modifier| modifier != INVALID_MODIFIER)
        .collect()
}

/// Whether a buffer GBM allocated plainly, with `modifier`, is one the compositor listed.
fn importable(listed: &[u64], modifier: u64) -> bool {
    listed.is_empty() || listed.contains(&modifier) || listed.contains(&INVALID_MODIFIER)
}

/// Blit coordinates of a swap: all of framebuffer 0 into all of the buffer, rows reversed.
fn flipped(width: i32, height: i32) -> ([i32; 4], [i32; 4]) {
    ([0, 0, width, height], [0, height, width, 0])
}

type Ptr = *mut c_void;

macro_rules! functions {
    ($name:ident, $lookup:path, { $($field:ident: $symbol:literal => fn($($argument:ty),*) $(-> $returns:ty)?;)* }) => {
        struct $name {
            $($field: unsafe extern "C" fn($($argument),*) $(-> $returns)?,)*
        }

        impl $name {
            fn load() -> Option<$name> {
                // SAFETY: each symbol has this signature in its library.
                Some($name {
                    $($field: unsafe {
                        std::mem::transmute::<*const c_void, unsafe extern "C" fn($($argument),*) $(-> $returns)?>($lookup($symbol)?)
                    },)*
                })
            }
        }
    };
}

fn libgbm(name: &CStr) -> Option<*const c_void> {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    let handle = *HANDLE.get_or_init(|| {
        // SAFETY: a C string; a missing library leaves the handle null.
        unsafe { libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) as usize }
    });
    if handle == 0 {
        return None;
    }
    // SAFETY: a handle from dlopen and a C string.
    let address = unsafe { libc::dlsym(handle as Ptr, name.as_ptr()) };
    (!address.is_null()).then_some(address.cast_const())
}

functions!(Gbm, libgbm, {
    create_device: c"gbm_create_device" => fn(i32) -> Ptr;
    destroy_device: c"gbm_device_destroy" => fn(Ptr);
    bo_create: c"gbm_bo_create" => fn(Ptr, u32, u32, u32, u32) -> Ptr;
    bo_create_with_modifiers: c"gbm_bo_create_with_modifiers" => fn(Ptr, u32, u32, u32, *const u64, u32) -> Ptr;
    bo_fd: c"gbm_bo_get_fd" => fn(Ptr) -> i32;
    bo_stride: c"gbm_bo_get_stride" => fn(Ptr) -> u32;
    bo_offset: c"gbm_bo_get_offset" => fn(Ptr, i32) -> u32;
    bo_modifier: c"gbm_bo_get_modifier" => fn(Ptr) -> u64;
    bo_planes: c"gbm_bo_get_plane_count" => fn(Ptr) -> i32;
    bo_destroy: c"gbm_bo_destroy" => fn(Ptr);
});

functions!(Gl, crate::egl::gles_or_egl, {
    initialize: c"eglInitialize" => fn(Ptr, *mut i32, *mut i32) -> u32;
    terminate: c"eglTerminate" => fn(Ptr) -> u32;
    query_string: c"eglQueryString" => fn(Ptr, i32) -> *const libc::c_char;
    choose_config: c"eglChooseConfig" => fn(Ptr, *const i32, *mut Ptr, i32, *mut i32) -> u32;
    current_context: c"eglGetCurrentContext" => fn() -> Ptr;
    create_image: c"eglCreateImageKHR" => fn(Ptr, Ptr, u32, Ptr, *const i32) -> Ptr;
    destroy_image: c"eglDestroyImageKHR" => fn(Ptr, Ptr) -> u32;
    create_sync: c"eglCreateSyncKHR" => fn(Ptr, u32, *const i32) -> Ptr;
    client_wait_sync: c"eglClientWaitSyncKHR" => fn(Ptr, Ptr, i32, u64) -> i32;
    destroy_sync: c"eglDestroySyncKHR" => fn(Ptr, Ptr) -> u32;
    get_string: c"glGetString" => fn(u32) -> *const u8;
    get_integer: c"glGetIntegerv" => fn(u32, *mut i32);
    is_enabled: c"glIsEnabled" => fn(u32) -> u8;
    enable: c"glEnable" => fn(u32);
    disable: c"glDisable" => fn(u32);
    finish: c"glFinish" => fn();
    gen_renderbuffers: c"glGenRenderbuffers" => fn(i32, *mut u32);
    delete_renderbuffers: c"glDeleteRenderbuffers" => fn(i32, *const u32);
    bind_renderbuffer: c"glBindRenderbuffer" => fn(u32, u32);
    image_renderbuffer: c"glEGLImageTargetRenderbufferStorageOES" => fn(u32, Ptr);
    gen_framebuffers: c"glGenFramebuffers" => fn(i32, *mut u32);
    delete_framebuffers: c"glDeleteFramebuffers" => fn(i32, *const u32);
    bind_framebuffer: c"glBindFramebuffer" => fn(u32, u32);
    attach_renderbuffer: c"glFramebufferRenderbuffer" => fn(u32, u32, u32, u32);
    check_framebuffer: c"glCheckFramebufferStatus" => fn(u32) -> u32;
    blit: c"glBlitFramebuffer" => fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32);
});

fn gl() -> Option<&'static Gl> {
    static GL: OnceLock<Option<Gl>> = OnceLock::new();
    GL.get_or_init(Gl::load).as_ref()
}

fn gbm() -> Option<&'static Gbm> {
    static GBM: OnceLock<Option<Gbm>> = OnceLock::new();
    GBM.get_or_init(Gbm::load).as_ref()
}

/// The GPU display: an EGL display on GBM's device, and the device buffers are allocated on.
struct Device {
    display: usize,
    gbm: usize,
    /// Whether `EGL_KHR_fence_sync` is there; otherwise a swap waits with `glFinish`.
    fences: bool,
}

static DEVICE: OnceLock<Option<Device>> = OnceLock::new();

fn has(list: *const libc::c_char, name: &str) -> bool {
    if list.is_null() {
        return false;
    }
    // SAFETY: EGL returns a C string.
    let list = unsafe { CStr::from_ptr(list) }.to_string_lossy();
    list.split(' ').any(|extension| extension == name)
}

/// Opens the first render node whose GBM device gives an EGL display that imports dmabufs and has
/// pbuffers, or None, and the caller uses Mesa's surfaceless platform. `UXDROID_EGL=software`
/// skips the GPU.
pub fn display() -> Option<Ptr> {
    DEVICE
        .get_or_init(|| {
            if std::env::var_os("UXDROID_EGL").is_some_and(|value| value == "readback") {
                return None;
            }
            (128..136).find_map(open)
        })
        .as_ref()
        .map(|device| device.display as Ptr)
}

fn open(node: u32) -> Option<Device> {
    let (gl, gbm) = (gl()?, gbm()?);
    let path = CString::new(format!("/dev/dri/renderD{node}")).ok()?;
    // SAFETY: a C string; the result is checked.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: GBM's and EGL's functions on objects made here; each failure undoes what was made.
    unsafe {
        let device = (gbm.create_device)(fd);
        if device.is_null() {
            libc::close(fd);
            return None;
        }
        let display = crate::egl::platform_display(PLATFORM_GBM, device);
        let (mut major, mut minor) = (0, 0);
        let usable =
            !display.is_null() && (gl.initialize)(display, &mut major, &mut minor) == 1 && {
                let extensions = (gl.query_string)(display, EXTENSIONS);
                let attributes = [
                    SURFACE_TYPE,
                    PBUFFER_BIT,
                    RENDERABLE_TYPE,
                    OPENGL_ES2_BIT,
                    NONE,
                ];
                let (mut config, mut count) = (std::ptr::null_mut(), 0);
                has(extensions, "EGL_EXT_image_dma_buf_import")
                    && has(extensions, "EGL_KHR_image_base")
                    && (gl.choose_config)(display, attributes.as_ptr(), &mut config, 1, &mut count)
                        == 1
                    && count > 0
            };
        if !usable {
            if !display.is_null() {
                (gl.terminate)(display);
            }
            (gbm.destroy_device)(device);
            libc::close(fd);
            return None;
        }
        let fences = has((gl.query_string)(display, EXTENSIONS), "EGL_KHR_fence_sync");
        let vendor = (gl.query_string)(display, VENDOR);
        let vendor = if vendor.is_null() {
            "unknown".into()
        } else {
            CStr::from_ptr(vendor).to_string_lossy()
        };
        uxdroid_bridge::report!(
            "uxdroid: EGL renders on the GPU through GBM on /dev/dri/renderD{node} ({vendor} EGL {major}.{minor})"
        );
        Some(Device {
            display: display as usize,
            gbm: device as usize,
            fences,
        })
    }
}

fn device() -> Option<&'static Device> {
    DEVICE.get().and_then(Option::as_ref)
}

/// One swapchain buffer: a GBM buffer object, its dmabuf, and the renderbuffer behind it.
struct Slot {
    bo: usize,
    fd: OwnedFd,
    stride: u32,
    offset: u32,
    modifier: u64,
    image: usize,
    renderbuffer: u32,
    framebuffer: u32,
    id: u64,
}

/// A window surface's swapchain, made on the context current at its first swap.
pub struct Chain {
    context: usize,
    width: i32,
    height: i32,
    /// The surface's layer, by its `Surface`'s `mNativeObject`, when the chain was made.
    surface: i64,
    listed: Vec<u64>,
    slots: Vec<Slot>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The GL state a swap changes, put back after it.
struct Saved {
    read: i32,
    draw: i32,
    renderbuffer: i32,
    scissor: bool,
    discard: bool,
}

impl Saved {
    /// # Safety
    /// A context must be current on the calling thread.
    unsafe fn take(gl: &Gl) -> Saved {
        let mut saved = Saved {
            read: 0,
            draw: 0,
            renderbuffer: 0,
            scissor: false,
            discard: false,
        };
        // SAFETY: as above.
        unsafe {
            (gl.get_integer)(GL_READ_FRAMEBUFFER_BINDING, &mut saved.read);
            (gl.get_integer)(GL_DRAW_FRAMEBUFFER_BINDING, &mut saved.draw);
            (gl.get_integer)(GL_RENDERBUFFER_BINDING, &mut saved.renderbuffer);
            saved.scissor = (gl.is_enabled)(GL_SCISSOR_TEST) != 0;
            saved.discard = (gl.is_enabled)(GL_RASTERIZER_DISCARD) != 0;
            // A blit is cut by the scissor, and by rasterizer discard on some drivers.
            (gl.disable)(GL_SCISSOR_TEST);
            (gl.disable)(GL_RASTERIZER_DISCARD);
        }
        saved
    }

    /// # Safety
    /// The context [`Saved::take`] ran on must be current.
    unsafe fn restore(&self, gl: &Gl) {
        // SAFETY: as above.
        unsafe {
            (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read as u32);
            (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.draw as u32);
            (gl.bind_renderbuffer)(GL_RENDERBUFFER, self.renderbuffer as u32);
            if self.scissor {
                (gl.enable)(GL_SCISSOR_TEST);
            }
            if self.discard {
                (gl.enable)(GL_RASTERIZER_DISCARD);
            }
        }
    }
}

impl Chain {
    /// Allocates and imports one buffer. Its renderbuffer and framebuffer belong to the current
    /// context, which must be the chain's; the caller saved the bindings this changes.
    unsafe fn slot(&self, gl: &Gl, gbm: &Gbm, device: &Device) -> Option<Slot> {
        let modifiers = allocation_modifiers(&self.listed);
        let (width, height) = (self.width as u32, self.height as u32);
        // SAFETY: GBM's and EGL's functions on objects made here; each failure undoes them.
        unsafe {
            let mut bo = std::ptr::null_mut();
            if !modifiers.is_empty() {
                bo = (gbm.bo_create_with_modifiers)(
                    device.gbm as Ptr,
                    width,
                    height,
                    ARGB8888,
                    modifiers.as_ptr(),
                    modifiers.len() as u32,
                );
            }
            if bo.is_null() {
                bo = (gbm.bo_create)(
                    device.gbm as Ptr,
                    width,
                    height,
                    ARGB8888,
                    GBM_BO_USE_RENDERING,
                );
            }
            if bo.is_null() {
                return None;
            }
            let modifier = (gbm.bo_modifier)(bo);
            let fd = (gbm.bo_fd)(bo);
            if fd < 0 || (gbm.bo_planes)(bo) != 1 || !importable(&self.listed, modifier) {
                if fd >= 0 {
                    libc::close(fd);
                }
                (gbm.bo_destroy)(bo);
                return None;
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let (stride, offset) = ((gbm.bo_stride)(bo), (gbm.bo_offset)(bo, 0));
            let mut attributes = vec![
                WIDTH,
                self.width,
                HEIGHT,
                self.height,
                LINUX_DRM_FOURCC,
                ARGB8888 as i32,
                PLANE0_FD,
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                PLANE0_OFFSET,
                offset as i32,
                PLANE0_PITCH,
                stride as i32,
            ];
            if modifier != INVALID_MODIFIER {
                attributes.extend([
                    PLANE0_MODIFIER_LO,
                    modifier as u32 as i32,
                    PLANE0_MODIFIER_HI,
                    (modifier >> 32) as u32 as i32,
                ]);
            }
            attributes.push(NONE);
            let image = (gl.create_image)(
                device.display as Ptr,
                std::ptr::null_mut(),
                LINUX_DMA_BUF,
                std::ptr::null_mut(),
                attributes.as_ptr(),
            );
            if image.is_null() {
                (gbm.bo_destroy)(bo);
                return None;
            }
            let (mut renderbuffer, mut framebuffer) = (0, 0);
            (gl.gen_renderbuffers)(1, &mut renderbuffer);
            (gl.bind_renderbuffer)(GL_RENDERBUFFER, renderbuffer);
            (gl.image_renderbuffer)(GL_RENDERBUFFER, image);
            (gl.gen_framebuffers)(1, &mut framebuffer);
            (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, framebuffer);
            (gl.attach_renderbuffer)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_RENDERBUFFER,
                renderbuffer,
            );
            if (gl.check_framebuffer)(GL_DRAW_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE {
                (gl.delete_framebuffers)(1, &framebuffer);
                (gl.delete_renderbuffers)(1, &renderbuffer);
                (gl.destroy_image)(device.display as Ptr, image);
                (gbm.bo_destroy)(bo);
                return None;
            }
            Some(Slot {
                bo: bo as usize,
                fd,
                stride,
                offset,
                modifier,
                image: image as usize,
                renderbuffer,
                framebuffer,
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            })
        }
    }

    /// Gives every buffer back: the window system's `wl_buffer`s, the GL names when the chain's
    /// context is current, the images and the buffer objects.
    pub fn release(self, presenter: Option<&Presenter>) {
        let (Some(gl), Some(gbm), Some(device)) = (gl(), gbm(), device()) else {
            return;
        };
        // SAFETY: EGL's and GBM's functions on this chain's own objects.
        unsafe {
            let current = (gl.current_context)() as usize == self.context;
            for slot in self.slots {
                if let Some(presenter) = presenter {
                    (presenter.forget)(self.surface, slot.id);
                }
                if current {
                    (gl.delete_framebuffers)(1, &slot.framebuffer);
                    (gl.delete_renderbuffers)(1, &slot.renderbuffer);
                }
                (gl.destroy_image)(device.display as Ptr, slot.image as Ptr);
                (gbm.bo_destroy)(slot.bo as Ptr);
                drop(slot.fd);
            }
        }
    }
}

/// Why a swap could not hand its frame over, when that is for good on this surface.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// Read this frame back; the next swap tries again.
    Now,
    /// Read every frame of this surface back.
    Always(&'static str),
}

/// Hands the current frame of `display`'s window surface, `width` by `height`, to the compositor
/// as a dmabuf, through `chain`, which it makes or remakes as needed. `surface` names the window's
/// layer. On [`Refused`] the caller reads the frame back.
///
/// # Safety
/// The surface must be current on the calling thread, drawn on `display`.
pub unsafe fn present(
    display: Ptr,
    chain: &mut Option<Chain>,
    surface: i64,
    width: i32,
    height: i32,
) -> Result<(), Refused> {
    let (Some(gl), Some(gbm), Some(device), Some(presenter)) =
        (gl(), gbm(), device(), crate::choreographer::presenter())
    else {
        return Err(Refused::Always("no GPU display or window system"));
    };
    if display as usize != device.display {
        return Err(Refused::Always("not the GPU display"));
    }
    // SAFETY: GL and EGL on the current context, as the caller promises.
    unsafe {
        let context = (gl.current_context)() as usize;
        let version = (gl.get_string)(GL_VERSION);
        if version.is_null()
            || !CStr::from_ptr(version.cast())
                .to_bytes()
                .starts_with(b"OpenGL ES 3")
        {
            return Err(Refused::Always("glBlitFramebuffer needs OpenGL ES 3"));
        }
        let stale = chain.as_ref().is_some_and(|chain| {
            (chain.context, chain.width, chain.height, chain.surface)
                != (context, width, height, surface)
        });
        if stale {
            chain.take().expect("checked").release(Some(presenter));
        }
        if chain.is_none() {
            let mut listed = [0u64; 64];
            let count = (presenter.modifiers)(surface, ARGB8888, listed.as_mut_ptr(), listed.len());
            if count < 0 {
                return Err(Refused::Now);
            }
            *chain = Some(Chain {
                context,
                width,
                height,
                surface,
                listed: listed[..(count as usize).min(listed.len())].to_vec(),
                slots: Vec::new(),
            });
        }
        let chain = chain.as_mut().expect("made above");
        let saved = Saved::take(gl);
        let result = draw(gl, gbm, device, presenter, chain, width, height);
        saved.restore(gl);
        result
    }
}

/// # Safety
/// As [`present`], with the GL state saved.
unsafe fn draw(
    gl: &Gl,
    gbm: &Gbm,
    device: &Device,
    presenter: &Presenter,
    chain: &mut Chain,
    width: i32,
    height: i32,
) -> Result<(), Refused> {
    // SAFETY: as above.
    unsafe {
        (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
        let mut samples = 0;
        (gl.get_integer)(GL_SAMPLE_BUFFERS, &mut samples);
        if samples > 0 {
            // A multisampled framebuffer 0 blits only to the same size and orientation.
            return Err(Refused::Always("framebuffer 0 is multisampled"));
        }
        let started = Instant::now();
        let at = loop {
            let busy: Vec<bool> = chain
                .slots
                .iter()
                .map(|slot| (presenter.busy)(chain.surface, slot.id) != 0)
                .collect();
            match choose(&busy, MAX_SLOTS) {
                Choice::Use(at) => break at,
                Choice::Grow => match chain.slot(gl, gbm, device) {
                    Some(slot) => {
                        chain.slots.push(slot);
                        break chain.slots.len() - 1;
                    }
                    None if chain.slots.is_empty() => {
                        return Err(Refused::Always("the GPU's buffers cannot be imported"));
                    }
                    // The ones there are will come back.
                    None => {}
                },
                Choice::Wait => {}
            }
            if started.elapsed() > WAIT_FOR_BUFFER {
                return Err(Refused::Now);
            }
            std::thread::sleep(Duration::from_micros(500));
        };
        let slot = &chain.slots[at];
        let (from, to) = flipped(width, height);
        (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, 0);
        (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, slot.framebuffer);
        (gl.blit)(
            from[0],
            from[1],
            from[2],
            from[3],
            to[0],
            to[1],
            to[2],
            to[3],
            GL_COLOR_BUFFER_BIT,
            GL_NEAREST,
        );
        // The compositor reads the buffer once the GPU wrote it.
        let display = device.display as Ptr;
        let fence = if device.fences {
            (gl.create_sync)(display, SYNC_FENCE, std::ptr::null())
        } else {
            std::ptr::null_mut()
        };
        if fence.is_null() {
            (gl.finish)();
        } else {
            let waited =
                (gl.client_wait_sync)(display, fence, SYNC_FLUSH_COMMANDS_BIT, 1_000_000_000);
            (gl.destroy_sync)(display, fence);
            if waited != CONDITION_SATISFIED {
                return Err(Refused::Now);
            }
        }
        let frame = Frame {
            fd: std::os::fd::AsRawFd::as_raw_fd(&slot.fd),
            width,
            height,
            stride: slot.stride,
            offset: slot.offset,
            format: ARGB8888,
            modifier: slot.modifier,
            id: slot.id,
        };
        if (presenter.present)(chain.surface, &frame) == SHOWN {
            Ok(())
        } else {
            Err(Refused::Now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_takes_a_free_buffer_then_grows_to_three_then_waits() {
        assert_eq!(choose(&[], MAX_SLOTS), Choice::Grow);
        assert_eq!(choose(&[true, false], MAX_SLOTS), Choice::Use(1));
        assert_eq!(choose(&[true, true], MAX_SLOTS), Choice::Grow);
        assert_eq!(choose(&[true, true, true], MAX_SLOTS), Choice::Wait);
        assert_eq!(choose(&[false, true, true], MAX_SLOTS), Choice::Use(0));
    }

    #[test]
    fn buffers_are_allocated_with_the_modifiers_the_compositor_imports() {
        let block = 0x0300_0000_00e0_8012;
        assert_eq!(
            allocation_modifiers(&[block, 0, INVALID_MODIFIER]),
            vec![block, 0]
        );
        assert!(allocation_modifiers(&[INVALID_MODIFIER]).is_empty());
        // A plain allocation fits a compositor that listed its modifier, the implicit one, or none.
        assert!(importable(&[block], block));
        assert!(!importable(&[0], block));
        assert!(importable(&[INVALID_MODIFIER], block));
        assert!(importable(&[], block));
    }

    #[test]
    fn a_swap_turns_the_frame_upside_down() {
        // GL's row 0 is the bottom; a dmabuf's first row is the top.
        assert_eq!(flipped(400, 700), ([0, 0, 400, 700], [0, 700, 400, 0]));
    }
}
