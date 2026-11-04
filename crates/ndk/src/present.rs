//! How a GPU frame of an EGL window surface reaches the compositor without copies: the window
//! system of `libuxdroid_system.so` takes it as a dmabuf, through [`Presenter`], whose address that
//! library hands over beside its frame clock (`VsyncListener.clock()`, version 2). These are host
//! functions between uxdroid's own libraries, not exports to app code, so no signature table
//! declares them; app code reaches them only through `eglSwapBuffers`.

/// A frame drawn into a one-plane dmabuf, as `zwp_linux_buffer_params_v1` takes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// The dmabuf; the window system duplicates what it keeps.
    pub fd: i32,
    pub width: i32,
    pub height: i32,
    pub stride: u32,
    pub offset: u32,
    /// A DRM fourcc. ARGB8888 is shown as XRGB8888 when the layer is opaque.
    pub format: u32,
    pub modifier: u64,
    /// The buffer's name, the same for every frame drawn into it, unique in the process.
    pub id: u64,
}

/// What [`Presenter::present`] answers.
pub const SHOWN: i32 = 0;
/// The frame cannot go as a dmabuf now, such as before the window opened or while its layer is
/// scaled: the caller reads it back instead.
pub const READ_BACK: i32 = 1;

/// The window system's side. `surface` is a `Surface`'s `mNativeObject`.
#[repr(C)]
pub struct Presenter {
    pub version: u32,
    /// Writes up to `capacity` modifiers the compositor imports `format` with and returns how many
    /// it has, or -1 when this surface's frames cannot go as dmabufs.
    pub modifiers:
        unsafe extern "C" fn(surface: i64, format: u32, out: *mut u64, capacity: usize) -> i32,
    /// Shows `frame` on the surface's layer: [`SHOWN`] or [`READ_BACK`].
    pub present: unsafe extern "C" fn(surface: i64, frame: *const Frame) -> i32,
    /// 1 while the compositor still reads buffer `id`, else 0.
    pub busy: extern "C" fn(surface: i64, id: u64) -> i32,
    /// Buffer `id` is gone: its `wl_buffer`s are destroyed.
    pub forget: extern "C" fn(surface: i64, id: u64),
}
