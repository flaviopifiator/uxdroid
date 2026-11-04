//! The Wayland side of an app's windows: one connection per process, whose one toplevel is the
//! display. The first window is the toplevel's surface; later ones, such as dialogs, popups and
//! menus, are subsurfaces of it, placed where the window manager put them, so the compositor
//! composes them and their frames stay apart. Pixels are shared memory, as milestone 3's window in
//! `crates/platform`. The seat's pointer, keyboard and touch events become input, tagged with the
//! surface they came from, and the connection is read without blocking, so the Looper can watch it
//! while the windows are idle and answer the compositor's pings. A configure with a new size and a
//! close are kept for the window manager to act on.
//!
//! A frame the GPU drew can also come as a dmabuf, through `zwp_linux_dmabuf_v1` when the compositor
//! offers it: on a window's own surface, or on a [`Plane`], a subsurface that shows one child layer,
//! such as a `SurfaceView`'s, without copies. Each dmabuf gets its `wl_buffer` once, and a buffer is
//! busy from its attach until the compositor releases it, so the GPU does not draw into what the
//! compositor still reads. Shared memory stays the path for everything else.

use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_region, wl_registry,
    wl_seat, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface, wl_touch,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::compose::Rect;
use crate::input::charmap::{self, Charmap};
use crate::input::keys::{self, Modifiers};
use crate::input::translate::Translator;
use crate::input::xkb::Keymap;
use crate::input::{self, Event};
use crate::vsync;

const BYTES_PER_PIXEL: i32 = 4;
const MAX_DIMENSION: i32 = 16384;
/// A keymap is a few tens of kilobytes; anything far larger is refused.
const MAX_KEYMAP: u32 = 4 << 20;
/// `xdg_toplevel.state.activated`.
const ACTIVATED: u32 = 4;

/// Where an input event goes: the surface it came from, or the focused window.
pub type Target = Option<u32>;

/// Seat input an agent sends: on a surface, in its coordinates, or for keys, to the focused window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Injected {
    TouchDown {
        surface: u32,
        x: f32,
        y: f32,
    },
    TouchUp {
        surface: u32,
    },
    Wheel {
        surface: u32,
        x: f32,
        y: f32,
        vertical: f32,
        horizontal: f32,
    },
    Key {
        evdev: u32,
        pressed: bool,
        shift: bool,
    },
}

/// The touch point agent taps use, far from the ids a compositor gives fingers.
const AGENT_TOUCH: i32 = i32::MAX - 1;
/// `KEY_LEFTSHIFT`.
const LEFT_SHIFT: u32 = 42;

#[derive(Default)]
struct State {
    configure_serial: Option<u32>,
    requested_size: Option<(i32, i32)>,
    closed: bool,
    activated: bool,
    /// The surface under the pointer, and under each touch point.
    pointer_surface: Option<u32>,
    touch_surfaces: Vec<(i32, u32)>,
    seat: Translator,
    /// The compositor's keymap, compiled when the first modifiers or key arrive.
    keymap_text: Option<String>,
    keymap: Option<Keymap>,
    modifiers: Modifiers,
    /// The last `wl_keyboard.modifiers`: depressed, latched, locked.
    masks: (u32, u32, u32),
    /// Pending scroll of the current pointer frame, in wheel steps.
    scroll: (f32, f32),
    discrete: bool,
    events: Vec<(Target, Event)>,
    /// Surfaces waiting for a frame callback.
    frames: Frames,
    /// What `zwp_linux_dmabuf_v1` imports.
    formats: Formats,
}

/// `DRM_FORMAT_ARGB8888` and `DRM_FORMAT_XRGB8888`, the fourcc codes of Wayland's two shm formats.
pub const ARGB8888: u32 = 0x3432_5241;
pub const XRGB8888: u32 = 0x3432_5258;
/// `DRM_FORMAT_MOD_LINEAR` and `DRM_FORMAT_MOD_INVALID`, the driver's implicit layout.
#[cfg(test)]
const LINEAR: u64 = 0;
pub const INVALID_MODIFIER: u64 = 0x00ff_ffff_ffff_ffff;

/// The format and modifier pairs the compositor imports dmabufs with, as it listed them.
#[derive(Default)]
struct Formats {
    pairs: Vec<(u32, u64)>,
}

impl Formats {
    fn add(&mut self, format: u32, modifier: u64) {
        if !self.pairs.contains(&(format, modifier)) {
            self.pairs.push((format, modifier));
        }
    }

    /// Whether the compositor listed anything yet.
    fn offered(&self) -> bool {
        !self.pairs.is_empty()
    }

    fn modifiers(&self, format: u32) -> Vec<u64> {
        self.pairs
            .iter()
            .filter(|(listed, _)| *listed == format)
            .map(|(_, modifier)| *modifier)
            .collect()
    }

    fn supports(&self, format: u32, modifier: u64) -> bool {
        self.pairs.contains(&(format, modifier))
    }
}

/// The `wl_buffer`s of one surface's dmabufs, by the id the GPU side gives each buffer and the
/// format it is shown as. `B` is the buffer, generic so the bookkeeping is tested without a
/// compositor.
struct Buffers<B> {
    entries: Vec<Entry<B>>,
}

impl<B> Default for Buffers<B> {
    fn default() -> Self {
        Buffers {
            entries: Vec::new(),
        }
    }
}

struct Entry<B> {
    id: u64,
    format: u32,
    buffer: B,
    /// Set at each attach, cleared by `wl_buffer.release`.
    busy: Arc<AtomicBool>,
}

/// The `wl_buffer`s a surface keeps: a swapchain of two or three, each perhaps in two formats.
const MAX_BUFFERS: usize = 8;

impl<B> Buffers<B> {
    fn get(&self, id: u64, format: u32) -> Option<&Entry<B>> {
        self.entries
            .iter()
            .find(|entry| entry.id == id && entry.format == format)
    }

    /// Keeps `buffer`, whose release clears `busy`, and returns the buffers it pushed out, oldest
    /// free ones first, for the caller to destroy.
    fn insert(&mut self, id: u64, format: u32, buffer: B, busy: Arc<AtomicBool>) -> Vec<B> {
        let mut dropped = Vec::new();
        while self.entries.len() >= MAX_BUFFERS {
            let Some(at) = self
                .entries
                .iter()
                .position(|entry| !entry.busy.load(Ordering::Acquire))
            else {
                break;
            };
            dropped.push(self.entries.remove(at).buffer);
        }
        self.entries.push(Entry {
            id,
            format,
            buffer,
            busy,
        });
        dropped
    }

    /// Buffer `id` was attached: busy until released.
    fn attached(&mut self, id: u64) {
        for entry in self.entries.iter().filter(|entry| entry.id == id) {
            entry.busy.store(true, Ordering::Release);
        }
    }

    fn busy(&self, id: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.id == id && entry.busy.load(Ordering::Acquire))
    }

    /// Takes out every `wl_buffer` of `id`, for the caller to destroy.
    fn forget(&mut self, id: u64) -> Vec<B> {
        let mut taken = Vec::new();
        let mut at = 0;
        while at < self.entries.len() {
            if self.entries[at].id == id {
                taken.push(self.entries.remove(at).buffer);
            } else {
                at += 1;
            }
        }
        taken
    }

    fn take_all(&mut self) -> Vec<B> {
        self.entries.drain(..).map(|entry| entry.buffer).collect()
    }
}

/// A frame the GPU drew into a one-plane dmabuf.
#[derive(Clone, Copy, Debug)]
pub struct Dmabuf {
    pub fd: RawFd,
    pub width: i32,
    pub height: i32,
    pub stride: u32,
    pub offset: u32,
    pub format: u32,
    pub modifier: u64,
    /// The GPU side's name for the buffer, the same for every frame drawn into it.
    pub id: u64,
}

/// A dmabuf's `wl_buffer`: the busy flag its release clears.
pub struct Release(Arc<AtomicBool>);

/// What the compositor answered a `zwp_linux_buffer_params_v1`: the buffer, or None when it
/// refused the dmabuf.
type Answer = Arc<Mutex<Option<Option<wl_buffer::WlBuffer>>>>;

/// The frame callbacks windows wait for. Each commit asks for one, so a callback the compositor
/// never answers, such as one asked before the surface was hidden, is replaced by the next frame's
/// instead of pinning the surface; the vsync counts each surface once and follows its newest.
///
/// A compositor keeps the callbacks of a hidden surface unanswered, and each is an object on both
/// sides, so a surface has at most [`MAX_PENDING_FRAMES`] of them: a frame past that asks for none,
/// and the vsync's own fallback paces the app until the compositor answers.
#[derive(Default)]
struct Frames {
    /// The newest callback each waiting surface asked for.
    newest: Vec<(u32, u32)>,
    /// Callbacks asked for and not answered yet, per surface.
    pending: Vec<(u32, u32)>,
}

/// Unanswered frame callbacks a surface may have.
const MAX_PENDING_FRAMES: u32 = 4;

impl Frames {
    /// A frame of `surface` is committed: the number of the callback to ask for, and whether the
    /// surface waits anew; None when it has as many unanswered callbacks as it may have.
    fn request(&mut self, surface: u32) -> Option<(u32, bool)> {
        let pending = match self.pending.iter_mut().find(|(key, _)| *key == surface) {
            Some((_, count)) => count,
            None => {
                self.pending.push((surface, 0));
                &mut self.pending.last_mut().expect("just pushed").1
            }
        };
        if *pending >= MAX_PENDING_FRAMES {
            return None;
        }
        *pending += 1;
        Some(
            match self.newest.iter_mut().find(|(key, _)| *key == surface) {
                Some((_, number)) => {
                    *number = number.wrapping_add(1);
                    (*number, false)
                }
                None => {
                    self.newest.push((surface, 0));
                    (0, true)
                }
            },
        )
    }

    /// The compositor answered callback `number` of `surface`: true when the surface stops waiting,
    /// which only its newest callback does.
    fn answer(&mut self, surface: u32, number: u32) -> bool {
        if let Some(at) = self.pending.iter().position(|(key, _)| *key == surface) {
            self.pending[at].1 = self.pending[at].1.saturating_sub(1);
            if self.pending[at].1 == 0 {
                self.pending.remove(at);
            }
        }
        match self
            .newest
            .iter()
            .position(|entry| *entry == (surface, number))
        {
            Some(at) => {
                self.newest.remove(at);
                true
            }
            None => false,
        }
    }

    /// The surface is gone, and its callbacks with it: true when it was waiting.
    fn forget(&mut self, surface: u32) -> bool {
        self.pending.retain(|(key, _)| *key != surface);
        let before = self.newest.len();
        self.newest.retain(|(key, _)| *key != surface);
        self.newest.len() != before
    }
}

impl State {
    fn update_meta(&mut self) {
        let (depressed, latched, locked) = self.masks;
        let meta = keys::meta_state(&self.modifiers, depressed, latched, locked);
        self.seat.set_meta_state(meta);
    }

    /// Compiles the keymap the compositor sent, whose characters then fill the key character map.
    fn compile_keymap(&mut self) {
        let Some(text) = self.keymap_text.take() else {
            return;
        };
        match Keymap::from_text(&text) {
            Some(mut keymap) => {
                self.modifiers = keymap.modifiers();
                charmap::set_seat(Charmap::from_keymap(&mut keymap));
                self.keymap = Some(keymap);
            }
            None => {
                uxdroid_bridge::report!(
                    "uxdroid: the compositor's keymap does not compile; keys type nothing"
                )
            }
        }
    }
}

/// Shared pixels and the Wayland buffer that shows them.
struct Pixels {
    mapping: *mut c_void,
    length: usize,
    buffer: wl_buffer::WlBuffer,
}

impl Drop for Pixels {
    fn drop(&mut self) {
        self.buffer.destroy();
        // SAFETY: `mapping` is this object's own mapping, and the buffer using it is destroyed.
        unsafe {
            libc::munmap(self.mapping, self.length);
        }
    }
}

/// The process's connection to the compositor and its one toplevel, which is the display.
struct Display {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    compositor: wl_compositor::WlCompositor,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: wl_shm::WlShm,
    shell: xdg_wm_base::XdgWmBase,
    /// `zwp_linux_dmabuf_v1`, version 3, when the compositor offers it.
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    _seat: Option<wl_seat::WlSeat>,
    toplevel: Option<wl_surface::WlSurface>,
    /// The toplevel's size, once a window has it.
    size: (i32, i32),
    /// Windows open, the toplevel's own included.
    windows: usize,
    /// The toplevel whose window closed while later windows still show on it as subsurfaces.
    orphan: Option<Orphan>,
}

/// A toplevel kept with its last frame after its window closed, because subsurfaces need a parent.
struct Orphan {
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    _pixels: Pixels,
}

// SAFETY: the orphan owns its mapping alone, and nothing reads it; it lives inside the display's
// mutex until it is destroyed.
unsafe impl Send for Orphan {}

impl Orphan {
    fn destroy(self) {
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

/// What a closing window does to the display's toplevel.
#[derive(Debug, PartialEq)]
enum Close {
    Keep,
    /// The toplevel's own window closed while others show on it: keep the toplevel, orphaned.
    Orphan,
    Destroy,
}

/// `toplevel_window`: the closing window is the toplevel's; `open_after`: windows left open after
/// it; `orphaned`: the toplevel already lost its own window.
fn on_close(toplevel_window: bool, open_after: usize, orphaned: bool) -> Close {
    match (toplevel_window, open_after, orphaned) {
        (true, 0, _) => Close::Destroy,
        (true, _, _) => Close::Orphan,
        (false, 0, true) => Close::Destroy,
        _ => Close::Keep,
    }
}

/// What the compositor asked of the display that the window manager must act on.
#[derive(Debug, Default, PartialEq)]
pub struct Requests {
    /// The toplevel's new size.
    pub resize: Option<(i32, i32)>,
    pub close: bool,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

fn display() -> MutexGuard<'static, Option<Display>> {
    DISPLAY.lock().unwrap_or_else(|p| p.into_inner())
}

impl Display {
    fn connect() -> Result<Display, Box<dyn Error>> {
        let connection = Connection::connect_to_env()?;
        let (globals, queue): (GlobalList, EventQueue<State>) = registry_queue_init(&connection)?;
        let handle = queue.handle();
        let compositor = globals.bind(&handle, 4..=6, ())?;
        let subcompositor = globals.bind(&handle, 1..=1, ()).ok();
        let shm = globals.bind(&handle, 1..=1, ())?;
        let shell = globals.bind(&handle, 1..=6, ())?;
        // A compositor without a seat shows the windows all the same, with no input.
        let seat = globals.bind(&handle, 1..=7, ()).ok();
        // Its formats come as events, read the first time a GPU frame asks for them. Version 3 lists
        // them with modifiers; later versions only through feedback objects.
        let dmabuf = globals.bind(&handle, 3..=3, ()).ok();
        Ok(Display {
            connection,
            queue,
            state: State::default(),
            compositor,
            subcompositor,
            shm,
            shell,
            dmabuf,
            _seat: seat,
            toplevel: None,
            size: (0, 0),
            windows: 0,
            orphan: None,
        })
    }
}

impl Display {
    /// The modifiers the compositor imports `format` with, reading its list the first time; None
    /// when it offers no dmabufs.
    fn modifiers(&mut self, format: u32) -> Result<Option<Vec<u64>>, Box<dyn Error>> {
        if self.dmabuf.is_none() {
            return Ok(None);
        }
        if !self.state.formats.offered() {
            self.queue.roundtrip(&mut self.state)?;
        }
        Ok(Some(self.state.formats.modifiers(format)))
    }

    /// Shows `frame` on `surface`: its `wl_buffer`, made the first time, attached with the whole
    /// buffer damaged, with a frame callback for the vsync.
    fn attach_dmabuf(
        &mut self,
        surface: &wl_surface::WlSurface,
        buffers: &mut Buffers<wl_buffer::WlBuffer>,
        frame: &Dmabuf,
    ) -> Result<(), Box<dyn Error>> {
        checked_size(frame.width, frame.height)?;
        if buffers.get(frame.id, frame.format).is_none() {
            let buffer = self.import(frame)?;
            let Some(Release(busy)) = buffer.data::<Release>() else {
                return Err("a dmabuf's wl_buffer has no release flag".into());
            };
            let busy = busy.clone();
            destroy_buffers(buffers.insert(frame.id, frame.format, buffer, busy));
        }
        let buffer = &buffers
            .get(frame.id, frame.format)
            .expect("made above")
            .buffer;
        let key = surface.id().protocol_id();
        if let Some((number, waits)) = self.state.frames.request(key) {
            surface.frame(&self.queue.handle(), (key, number));
            if waits {
                vsync::frame_requested();
            }
        }
        surface.attach(Some(buffer), 0, 0);
        surface.damage_buffer(0, 0, frame.width, frame.height);
        buffers.attached(frame.id);
        surface.commit();
        self.queue.flush()?;
        Ok(())
    }

    /// A `wl_buffer` for `frame`'s dmabuf, or an error when the compositor refuses it.
    fn import(&mut self, frame: &Dmabuf) -> Result<wl_buffer::WlBuffer, Box<dyn Error>> {
        let Some(dmabuf) = &self.dmabuf else {
            return Err("the compositor offers no zwp_linux_dmabuf_v1".into());
        };
        if frame.fd < 0 {
            return Err("no dmabuf".into());
        }
        let answer: Answer = Arc::new(Mutex::new(None));
        let params = dmabuf.create_params(&self.queue.handle(), answer.clone());
        // SAFETY: the caller keeps the descriptor open for the call; the request only sends it.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(frame.fd) };
        params.add(
            fd,
            0,
            frame.offset,
            frame.stride,
            (frame.modifier >> 32) as u32,
            frame.modifier as u32,
        );
        params.create(
            frame.width,
            frame.height,
            frame.format,
            zwp_linux_buffer_params_v1::Flags::empty(),
        );
        while answer.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        params.destroy();
        let answered = answer.lock().unwrap_or_else(|p| p.into_inner()).take();
        match answered.flatten() {
            Some(buffer) => Ok(buffer),
            None => Err(format!(
                "the compositor refused a {}x{} dmabuf of format {:#x}, modifier {:#x}",
                frame.width, frame.height, frame.format, frame.modifier
            )
            .into()),
        }
    }
}

/// The modifiers the compositor imports dmabufs of `format` with; None when it offers no dmabufs
/// or no window is open yet.
pub fn dmabuf_modifiers(format: u32) -> Option<Vec<u64>> {
    let mut display = display();
    let display = display.as_mut()?;
    match display.modifiers(format) {
        Ok(modifiers) => modifiers,
        Err(error) => {
            uxdroid_bridge::report!(
                "uxdroid: cannot read the compositor's dmabuf formats: {error}"
            );
            None
        }
    }
}

/// Whether the compositor listed `format` with `modifier`.
pub fn dmabuf_supports(format: u32, modifier: u64) -> bool {
    display()
        .as_ref()
        .is_some_and(|display| display.state.formats.supports(format, modifier))
}

/// Runs `f` on the display, connecting to the compositor the first time.
fn with_display<T>(
    f: impl FnOnce(&mut Display) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let mut display = display();
    if display.is_none() {
        *display = Some(Display::connect()?);
    }
    match display.as_mut() {
        Some(display) => f(display),
        None => Err("no display".into()),
    }
}

/// The connection's descriptor, readable when the compositor sent something; none before the first
/// window.
pub fn fd() -> Option<RawFd> {
    display()
        .as_ref()
        .map(|display| display.connection.backend().poll_fd().as_raw_fd())
}

/// Reads and handles whatever the compositor sent, without blocking.
pub fn dispatch() -> Result<(), Box<dyn Error>> {
    let mut display = display();
    let Some(display) = display.as_mut() else {
        return Ok(());
    };
    display.queue.dispatch_pending(&mut display.state)?;
    if let Some(guard) = display.queue.prepare_read() {
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
    }
    display.queue.dispatch_pending(&mut display.state)?;
    display.queue.flush()?;
    Ok(())
}

/// Input that arrived since the last call, in order, with the surface each came from.
pub fn take_events() -> Vec<(Target, Event)> {
    display()
        .as_mut()
        .map(|display| std::mem::take(&mut display.state.events))
        .unwrap_or_default()
}

/// Puts agent input among the seat's, as the compositor would have sent it: the same translation
/// makes it Android events, so the app cannot tell the two apart. The caller routes it.
pub fn inject(input: Injected) -> Result<(), String> {
    let mut display = display();
    let Some(display) = display.as_mut() else {
        return Err("the app shows no window".into());
    };
    let state = &mut display.state;
    let now = input::now();
    let shift = keys::META_SHIFT_ON | keys::META_SHIFT_LEFT_ON;
    let (target, events) = match input {
        Injected::TouchDown { surface, x, y } => {
            (Some(surface), state.seat.touch_down(AGENT_TOUCH, x, y, now))
        }
        Injected::TouchUp { surface } => (Some(surface), state.seat.touch_up(AGENT_TOUCH, now)),
        Injected::Wheel {
            surface,
            x,
            y,
            vertical,
            horizontal,
        } => (
            Some(surface),
            state.seat.wheel_at(x, y, vertical, horizontal, now),
        ),
        // Shift goes down before the key and up after it, with its meta state on the key's events,
        // as a compositor's modifiers event would set it.
        Injected::Key {
            evdev,
            pressed,
            shift: shifted,
        } => {
            let mut events = Vec::new();
            if shifted && pressed {
                events.extend(state.seat.key(LEFT_SHIFT, true, now));
            }
            if shifted {
                state.seat.set_meta_state(state.seat.meta_state() | shift);
            }
            events.extend(state.seat.key(evdev, pressed, now));
            if shifted && !pressed {
                events.extend(state.seat.key(LEFT_SHIFT, false, now));
            }
            state.update_meta();
            (None, events)
        }
    };
    state
        .events
        .extend(events.into_iter().map(|event| (target, event)));
    Ok(())
}

/// The display's size: its toplevel's, none before the first window.
pub fn display_size() -> Option<(i32, i32)> {
    display()
        .as_ref()
        .filter(|display| display.toplevel.is_some())
        .map(|display| display.size)
}

/// A new size or a close the compositor asked for since the last call.
pub fn take_requests() -> Requests {
    let mut display = display();
    let Some(display) = display.as_mut() else {
        return Requests::default();
    };
    let mut requests = Requests::default();
    if let Some(size) = display.state.requested_size.take()
        && display.toplevel.is_some()
        && size != display.size
    {
        display.size = size;
        requests.resize = Some(size);
    }
    requests.close = std::mem::take(&mut display.state.closed);
    requests
}

enum Role {
    Toplevel(xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel),
    Sub(wl_subsurface::WlSubsurface),
}

/// One window: a surface with its frame's pixels.
pub struct Window {
    surface: wl_surface::WlSurface,
    role: Role,
    /// Taken only when the window closes and its toplevel keeps the last frame.
    pixels: Option<Pixels>,
    width: i32,
    height: i32,
    /// XRGB8888 when opaque, else ARGB8888 with premultiplied alpha, so the compositor shows what is
    /// below the window's transparent parts, such as a dialog's shadow.
    opaque: bool,
    /// The `wl_buffer`s of dmabufs this surface showed.
    dmabufs: Buffers<wl_buffer::WlBuffer>,
}

/// The pixel format of a window's buffer.
fn format(opaque: bool) -> wl_shm::Format {
    if opaque {
        wl_shm::Format::Xrgb8888
    } else {
        wl_shm::Format::Argb8888
    }
}

/// The pixels of a frame, 0xAARRGGBB, row after row.
pub struct Frame<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

fn checked_size(width: i32, height: i32) -> Result<(), Box<dyn Error>> {
    if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height) {
        return Err(format!("bad window size {width}x{height}").into());
    }
    Ok(())
}

fn allocate(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<State>,
    width: i32,
    height: i32,
    opaque: bool,
) -> Result<Pixels, Box<dyn Error>> {
    checked_size(width, height)?;
    let stride = width * BYTES_PER_PIXEL;
    let size = stride * height;
    // SAFETY: the name is a valid C string, and the result is checked.
    let fd = unsafe { libc::memfd_create(c"uxdroid-window".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: memfd_create just returned this descriptor, and nothing else owns it.
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64)?;
    // SAFETY: a new shared mapping of `size` bytes of this file touches no existing memory.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().into());
    }
    let pool = shm.create_pool(file.as_fd(), size, queue, ());
    let buffer = pool.create_buffer(0, width, height, stride, format(opaque), queue, ());
    pool.destroy();
    Ok(Pixels {
        mapping,
        length: size as usize,
        buffer,
    })
}

impl Window {
    /// Opens the display's toplevel, or when it is open, a subsurface of it at `(x, y)` on the
    /// display. Either has exactly this size; a size the compositor asks for comes as a request.
    /// A window that is not `opaque` has an alpha channel.
    pub fn open(
        title: &str,
        width: i32,
        height: i32,
        x: i32,
        y: i32,
        opaque: bool,
    ) -> Result<Window, Box<dyn Error>> {
        with_display(|display| {
            let handle = display.queue.handle();
            let surface = display.compositor.create_surface(&handle, ());
            if let (Some(parent), Some(subcompositor)) = (&display.toplevel, &display.subcompositor)
            {
                checked_size(width, height)?;
                let subsurface = subcompositor.get_subsurface(&surface, parent, &handle, ());
                // Each window shows its frames when it posts them, not when the display does.
                subsurface.set_desync();
                subsurface.set_position(x, y);
                parent.commit();
                let pixels = allocate(&display.shm, &handle, width, height, opaque)?;
                display.windows += 1;
                return Ok(Window {
                    surface,
                    role: Role::Sub(subsurface),
                    pixels: Some(pixels),
                    width,
                    height,
                    opaque,
                    dmabufs: Buffers::default(),
                });
            }
            if display.toplevel.is_some() {
                return Err("the compositor has no wl_subcompositor for a second window".into());
            }
            let xdg_surface = display.shell.get_xdg_surface(&surface, &handle, ());
            let toplevel = xdg_surface.get_toplevel(&handle, ());
            toplevel.set_title(title.to_string());
            toplevel.set_app_id(title.to_string());
            surface.commit();
            let state = &mut display.state;
            state.configure_serial = None;
            while state.configure_serial.is_none() && !state.closed {
                display.queue.blocking_dispatch(state)?;
            }
            // The first frame has the size the app asked for. A size the compositor chose stays
            // requested, so the window manager lays the windows out again for it.
            checked_size(width, height)?;
            let pixels = allocate(&display.shm, &handle, width, height, opaque)?;
            display.toplevel = Some(surface.clone());
            display.size = (width, height);
            display.windows += 1;
            Ok(Window {
                surface,
                role: Role::Toplevel(xdg_surface, toplevel),
                pixels: Some(pixels),
                width,
                height,
                opaque,
                dmabufs: Buffers::default(),
            })
        })
    }

    /// Whether this is the toplevel's own window, at the display's origin.
    pub fn is_toplevel(&self) -> bool {
        matches!(self.role, Role::Toplevel(..))
    }

    /// The id input events name this window's surface by.
    pub fn key(&self) -> u32 {
        self.surface.id().protocol_id()
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// Takes a new buffer of another size; the next frame fills it.
    pub fn resize(&mut self, width: i32, height: i32) -> Result<(), Box<dyn Error>> {
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        let pixels = with_display(|display| {
            let handle = display.queue.handle();
            let pixels = allocate(&display.shm, &handle, width, height, self.opaque)?;
            if matches!(self.role, Role::Toplevel(..)) {
                display.size = (width, height);
            }
            Ok(pixels)
        })?;
        self.pixels = Some(pixels);
        (self.width, self.height) = (width, height);
        Ok(())
    }

    /// Follows the window's opacity: a new buffer of the other format, whose frame is all to be
    /// drawn again. Returns whether it changed.
    pub fn set_opaque(&mut self, opaque: bool) -> Result<bool, Box<dyn Error>> {
        if opaque == self.opaque {
            return Ok(false);
        }
        let (width, height) = (self.width, self.height);
        let pixels = with_display(|display| {
            let handle = display.queue.handle();
            allocate(&display.shm, &handle, width, height, opaque)
        })?;
        self.pixels = Some(pixels);
        self.opaque = opaque;
        Ok(true)
    }

    /// Moves a subsurface to `(x, y)` on the display. The toplevel does not move.
    pub fn place(&self, x: i32, y: i32) -> Result<(), Box<dyn Error>> {
        if let Role::Sub(subsurface) = &self.role {
            with_display(|display| {
                subsurface.set_position(x, y);
                // A subsurface's position applies with its parent's next commit.
                if let Some(parent) = &display.toplevel {
                    parent.commit();
                }
                display.queue.flush()?;
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn frame(&mut self) -> Frame<'_> {
        let mapping = self.pixels.as_ref().expect("an open window has pixels");
        // SAFETY: the mapping holds `length` bytes aligned to a page, and lives as long as `self`.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(mapping.mapping.cast::<u32>(), mapping.length / 4)
        };
        Frame {
            pixels,
            width: self.width,
            height: self.height,
        }
    }

    /// Shows the frame, telling the compositor that only `damage` changed, and asks for a frame
    /// callback, which paces the app's vsync.
    pub fn present(&mut self, damage: Rect) -> Result<(), Box<dyn Error>> {
        let buffer = &self
            .pixels
            .as_ref()
            .expect("an open window has pixels")
            .buffer;
        with_display(|display| {
            let key = self.key();
            if let Some((number, waits)) = display.state.frames.request(key) {
                self.surface.frame(&display.queue.handle(), (key, number));
                if waits {
                    vsync::frame_requested();
                }
            }
            self.surface.attach(Some(buffer), 0, 0);
            self.surface
                .damage_buffer(damage.left, damage.top, damage.width(), damage.height());
            self.surface.commit();
            display.queue.flush()?;
            // A round trip proves the compositor took the frame before the caller goes on.
            display.queue.roundtrip(&mut display.state)?;
            Ok(())
        })
    }

    /// Shows a dmabuf the GPU drew as this window's frame, in place of its shared memory.
    pub fn present_dmabuf(&mut self, frame: &Dmabuf) -> Result<(), Box<dyn Error>> {
        let (surface, buffers) = (&self.surface, &mut self.dmabufs);
        with_display(|display| display.attach_dmabuf(surface, buffers, frame))
    }

    /// Whether the compositor still reads dmabuf `id`, after reading what it sent.
    pub fn dmabuf_busy(&self, id: u64) -> bool {
        let _ = dispatch();
        self.dmabufs.busy(id)
    }

    /// Forgets dmabuf `id`, which the GPU side no longer draws into.
    pub fn forget_dmabuf(&mut self, id: u64) {
        destroy_buffers(self.dmabufs.forget(id));
    }

    /// Commits the surface with nothing new, which applies its subsurfaces' new positions.
    fn commit(&self) -> Result<(), Box<dyn Error>> {
        with_display(|display| {
            self.surface.commit();
            display.queue.flush()?;
            Ok(())
        })
    }

    /// A subsurface of this window for one child layer's GPU frames: shown above the window until
    /// placed, without input, so the window under it keeps every event.
    pub fn plane(&self) -> Result<Plane, Box<dyn Error>> {
        let Some(subcompositor) = with_display(|display| Ok(display.subcompositor.clone()))? else {
            return Err("the compositor has no wl_subcompositor".into());
        };
        with_display(|display| {
            let handle = display.queue.handle();
            let surface = display.compositor.create_surface(&handle, ());
            let subsurface = subcompositor.get_subsurface(&surface, &self.surface, &handle, ());
            subsurface.set_desync();
            let region = display.compositor.create_region(&handle, ());
            surface.set_input_region(Some(&region));
            region.destroy();
            Ok(Plane {
                surface,
                subsurface,
                buffers: Buffers::default(),
                place: None,
            })
        })
    }
}

fn destroy_buffers(buffers: Vec<wl_buffer::WlBuffer>) {
    for buffer in buffers {
        buffer.destroy();
    }
}

/// A subsurface showing one child layer's GPU frames in its window, as SurfaceFlinger shows a
/// `SurfaceView`'s buffers on a layer of their own.
pub struct Plane {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    buffers: Buffers<wl_buffer::WlBuffer>,
    /// Where it was last placed: position and whether below its window.
    place: Option<(i32, i32, bool)>,
}

impl Plane {
    /// Puts the plane at `(x, y)` in `window`, below or above the window's own frame. The new place
    /// applies with the window's next commit, which this makes when it changed.
    pub fn place(
        &mut self,
        window: &Window,
        x: i32,
        y: i32,
        below: bool,
    ) -> Result<(), Box<dyn Error>> {
        if self.place == Some((x, y, below)) {
            return Ok(());
        }
        self.subsurface.set_position(x, y);
        if below {
            self.subsurface.place_below(&window.surface);
        } else {
            self.subsurface.place_above(&window.surface);
        }
        self.place = Some((x, y, below));
        window.commit()
    }

    pub fn present_dmabuf(&mut self, frame: &Dmabuf) -> Result<(), Box<dyn Error>> {
        let (surface, buffers) = (&self.surface, &mut self.buffers);
        with_display(|display| display.attach_dmabuf(surface, buffers, frame))
    }

    pub fn dmabuf_busy(&self, id: u64) -> bool {
        let _ = dispatch();
        self.buffers.busy(id)
    }

    pub fn forget_dmabuf(&mut self, id: u64) {
        destroy_buffers(self.buffers.forget(id));
    }
}

// SAFETY: Wayland proxies may be used from any thread; the plane is used behind its layer's mutex.
unsafe impl Send for Plane {}

impl Drop for Plane {
    fn drop(&mut self) {
        let mut guard = display();
        let Some(display) = guard.as_mut() else {
            return;
        };
        if display.state.frames.forget(self.surface.id().protocol_id()) {
            vsync::frame_dropped();
        }
        destroy_buffers(self.buffers.take_all());
        self.subsurface.destroy();
        self.surface.destroy();
        let _ = display.queue.flush();
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let mut guard = display();
        let Some(display) = guard.as_mut() else {
            return;
        };
        let key = self.key();
        if display.state.frames.forget(key) {
            vsync::frame_dropped();
        }
        destroy_buffers(self.dmabufs.take_all());
        display.windows = display.windows.saturating_sub(1);
        let close = on_close(
            matches!(self.role, Role::Toplevel(..)),
            display.windows,
            display.orphan.is_some(),
        );
        match &self.role {
            Role::Toplevel(xdg_surface, toplevel) if close == Close::Orphan => {
                // Later windows are subsurfaces of this surface, so it stays with its last frame.
                display.orphan = Some(Orphan {
                    surface: self.surface.clone(),
                    xdg_surface: xdg_surface.clone(),
                    toplevel: toplevel.clone(),
                    _pixels: self.pixels.take().expect("an open window has pixels"),
                });
            }
            Role::Toplevel(xdg_surface, toplevel) => {
                toplevel.destroy();
                xdg_surface.destroy();
                self.surface.destroy();
                display.toplevel = None;
            }
            Role::Sub(subsurface) => {
                subsurface.destroy();
                self.surface.destroy();
                if close == Close::Destroy
                    && let Some(orphan) = display.orphan.take()
                {
                    orphan.destroy();
                    display.toplevel = None;
                }
            }
        }
        let _ = display.queue.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configure_serial = Some(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                if width > 0 && height > 0 {
                    // A compositor can ask for anything, and the window takes what it can hold.
                    state.requested_size =
                        Some((width.min(MAX_DIMENSION), height.min(MAX_DIMENSION)));
                }
                // An activated toplevel is the focused window, as Android's window manager
                // focuses the window in front.
                let activated = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|state| u32::from_ne_bytes(*state) == ACTIVATED);
                if activated != state.activated {
                    state.activated = activated;
                    if !activated {
                        let released = state.seat.keyboard_leave(input::now());
                        state
                            .events
                            .extend(released.into_iter().map(|event| (None, event)));
                    }
                    state.events.push((None, Event::Focus(activated)));
                }
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            // Each device is asked for once, when the seat first reports it.
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(handle, ());
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(handle, ());
            }
            if capabilities.contains(wl_seat::Capability::Touch) {
                seat.get_touch(handle, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let now = input::now();
        // Events before a leave belong to the surface it leaves.
        let mut surface = state.pointer_surface;
        let events = match event {
            wl_pointer::Event::Enter {
                surface: entered,
                surface_x,
                surface_y,
                ..
            } => {
                surface = Some(entered.id().protocol_id());
                state.pointer_surface = surface;
                state
                    .seat
                    .pointer_enter(surface_x as f32, surface_y as f32, now)
            }
            wl_pointer::Event::Leave { .. } => {
                state.pointer_surface = None;
                state.seat.pointer_leave(now)
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => state
                .seat
                .pointer_motion(surface_x as f32, surface_y as f32, now),
            wl_pointer::Event::Button {
                button,
                state: pressed,
                ..
            } => state.seat.pointer_button(
                button,
                pressed == WEnum::Value(wl_pointer::ButtonState::Pressed),
                now,
            ),
            wl_pointer::Event::AxisDiscrete { axis, discrete } => {
                state.discrete = true;
                state.add_scroll(axis, discrete as f32);
                Vec::new()
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                state.discrete = true;
                state.add_scroll(axis, value120 as f32 / 120.0);
                Vec::new()
            }
            // One wheel step is 10 units of motion on most compositors.
            wl_pointer::Event::Axis { axis, value, .. } if !state.discrete => {
                state.add_scroll(axis, value as f32 / 10.0);
                Vec::new()
            }
            wl_pointer::Event::Frame => {
                let (vertical, horizontal) = std::mem::take(&mut state.scroll);
                state.discrete = false;
                state.seat.pointer_scroll(vertical, horizontal, now)
            }
            _ => Vec::new(),
        };
        state
            .events
            .extend(events.into_iter().map(|event| (surface, event)));
    }
}

impl State {
    fn add_scroll(&mut self, axis: WEnum<wl_pointer::Axis>, steps: f32) {
        match axis {
            WEnum::Value(wl_pointer::Axis::VerticalScroll) => self.scroll.0 += steps,
            WEnum::Value(wl_pointer::Axis::HorizontalScroll) => self.scroll.1 += steps,
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let now = input::now();
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } if size <= MAX_KEYMAP => {
                // Read from the start: the compositor's own writes may have moved the shared offset.
                let mut bytes = vec![0; size as usize];
                if File::from(fd).read_exact_at(&mut bytes, 0).is_ok() {
                    // The keymap ends with a NUL that xkb does not want.
                    let text = String::from_utf8_lossy(&bytes);
                    state.keymap_text = Some(text.trim_end_matches('\0').to_string());
                    state.keymap = None;
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                let released = state.seat.keyboard_leave(now);
                state
                    .events
                    .extend(released.into_iter().map(|event| (None, event)));
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                ..
            } => {
                // Compiling waits for the first key, so a window nobody types into never pays it.
                state.masks = (mods_depressed, mods_latched, mods_locked);
                state.update_meta();
            }
            wl_keyboard::Event::Key {
                key,
                state: pressed,
                ..
            } => {
                if state.keymap.is_none() && state.keymap_text.is_some() {
                    state.compile_keymap();
                    state.update_meta();
                }
                let events = state.seat.key(
                    key,
                    pressed == WEnum::Value(wl_keyboard::KeyState::Pressed),
                    now,
                );
                state
                    .events
                    .extend(events.into_iter().map(|event| (None, event)));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let now = input::now();
        // A touch belongs to the surface it went down on; a frame, to the first touch's surface.
        let surface_of = |state: &State, id: Option<i32>| {
            state
                .touch_surfaces
                .iter()
                .find(|(touch, _)| id.is_none_or(|id| *touch == id))
                .map(|(_, surface)| *surface)
        };
        let (surface, events) = match event {
            wl_touch::Event::Down {
                id, surface, x, y, ..
            } => {
                state.touch_surfaces.retain(|(touch, _)| *touch != id);
                state.touch_surfaces.push((id, surface.id().protocol_id()));
                (
                    surface_of(state, Some(id)),
                    state.seat.touch_down(id, x as f32, y as f32, now),
                )
            }
            wl_touch::Event::Up { id, .. } => {
                let surface = surface_of(state, Some(id));
                let events = state.seat.touch_up(id, now);
                state.touch_surfaces.retain(|(touch, _)| *touch != id);
                (surface, events)
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                state.seat.touch_motion(id, x as f32, y as f32);
                (None, Vec::new())
            }
            wl_touch::Event::Frame => (surface_of(state, None), state.seat.touch_frame(now)),
            wl_touch::Event::Cancel => {
                let surface = surface_of(state, None);
                state.touch_surfaces.clear();
                (surface, state.seat.touch_cancel(now))
            }
            _ => (None, Vec::new()),
        };
        state
            .events
            .extend(events.into_iter().map(|event| (surface, event)));
    }
}

impl Dispatch<wl_callback::WlCallback, (u32, u32)> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        &(surface, number): &(u32, u32),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && state.frames.answer(surface, number)
        {
            vsync::frame_done();
        }
    }
}

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_dmabuf_v1::Event::Format { format } => {
                state.formats.add(format, INVALID_MODIFIER)
            }
            zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } => state.formats.add(
                format,
                (u64::from(modifier_hi) << 32) | u64::from(modifier_lo),
            ),
            _ => {}
        }
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, Answer> for State {
    fn event(
        _: &mut Self,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        answer: &Answer,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let answered = match event {
            zwp_linux_buffer_params_v1::Event::Created { buffer } => Some(buffer),
            zwp_linux_buffer_params_v1::Event::Failed => None,
            _ => return,
        };
        *answer.lock().unwrap_or_else(|p| p.into_inner()) = Some(answered);
    }

    event_created_child!(State, zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (
            wl_buffer::WlBuffer,
            Release(Arc::new(AtomicBool::new(false)))
        ),
    ]);
}

impl Dispatch<wl_buffer::WlBuffer, Release> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        Release(busy): &Release,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Release);
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(State: ignore wl_subsurface::WlSubsurface);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_commit_asks_for_a_frame_and_the_newest_ends_the_wait() {
        let mut frames = Frames::default();
        assert_eq!(frames.request(7), Some((0, true)));
        // A second frame before the first was answered asks again, and the surface counts once.
        assert_eq!(frames.request(7), Some((1, false)));
        assert_eq!(frames.request(9), Some((0, true)));
        // The stale callback, never answered or answered late, does not end the wait.
        assert!(!frames.answer(7, 0));
        assert!(frames.answer(7, 1));
        assert!(!frames.answer(7, 1), "answered once");
        assert!(frames.forget(9));
        assert!(!frames.forget(9));
        assert_eq!(frames.request(7), Some((0, true)));
    }

    #[test]
    fn a_surface_whose_callbacks_go_unanswered_asks_for_no_more_than_a_few() {
        let mut frames = Frames::default();
        for number in 0..MAX_PENDING_FRAMES {
            assert!(frames.request(7).is_some(), "callback {number}");
        }
        // Hidden, as the compositor answers nothing: no more callbacks pile up.
        assert_eq!(frames.request(7), None);
        assert_eq!(frames.request(7), None);
        // One answered, even a stale one, makes room for one more.
        assert!(!frames.answer(7, 0));
        assert_eq!(frames.request(7), Some((MAX_PENDING_FRAMES, false)));
        assert_eq!(frames.request(7), None);
        // A surface gone takes its count with it.
        frames.forget(7);
        assert_eq!(frames.request(7), Some((0, true)));
    }

    #[test]
    fn the_compositor_imports_the_pairs_it_listed() {
        let mut formats = Formats::default();
        assert!(!formats.offered(), "no zwp_linux_dmabuf_v1 yet");
        formats.add(ARGB8888, 0x0300_0000_00e0_8012);
        formats.add(ARGB8888, LINEAR);
        formats.add(XRGB8888, LINEAR);
        // A format listed without modifiers takes the driver's implicit one.
        formats.add(XRGB8888, INVALID_MODIFIER);
        formats.add(ARGB8888, LINEAR);
        assert!(formats.offered());
        assert_eq!(
            formats.modifiers(ARGB8888),
            vec![0x0300_0000_00e0_8012, LINEAR]
        );
        assert!(formats.supports(XRGB8888, LINEAR));
        assert!(!formats.supports(XRGB8888, 0x0300_0000_00e0_8012));
        assert!(formats.supports(XRGB8888, INVALID_MODIFIER));
        assert!(
            formats.modifiers(0x3231_5258).is_empty(),
            "XR12, never listed"
        );
    }

    #[test]
    fn a_buffer_is_busy_from_its_attach_until_the_compositor_releases_it() {
        let mut buffers: Buffers<u8> = Buffers::default();
        assert!(buffers.get(1, ARGB8888).is_none());
        let release = Arc::new(AtomicBool::new(false));
        assert!(buffers.insert(1, ARGB8888, 10, release.clone()).is_empty());
        // Not attached yet: free.
        assert!(!buffers.busy(1));
        buffers.attached(1);
        assert!(buffers.busy(1));
        assert!(!buffers.busy(2), "an unknown buffer is not busy");
        release.store(false, Ordering::Release);
        assert!(!buffers.busy(1));
        // The same memory shown as another format is another wl_buffer.
        assert!(buffers.get(1, XRGB8888).is_none());
        buffers.insert(1, XRGB8888, 11, Arc::default());
        buffers.attached(1);
        assert!(buffers.busy(1), "busy while either of its wl_buffers is");
        assert_eq!(buffers.forget(1), vec![10, 11]);
        assert!(buffers.get(1, ARGB8888).is_none());
    }

    #[test]
    fn a_surface_keeps_a_bounded_number_of_buffers_and_drops_free_ones_first() {
        let mut buffers: Buffers<u64> = Buffers::default();
        for id in 0..MAX_BUFFERS as u64 {
            buffers.insert(id, ARGB8888, id, Arc::default());
            buffers.attached(id);
        }
        buffers
            .get(0, ARGB8888)
            .unwrap()
            .busy
            .store(false, Ordering::Release);
        // A new one pushes out the oldest free buffer, never one the compositor still reads.
        let dropped = buffers.insert(99, ARGB8888, 99, Arc::default());
        assert_eq!(dropped, vec![0]);
        assert!(buffers.get(0, ARGB8888).is_none());
        assert!(buffers.get(1, ARGB8888).is_some());
    }

    #[test]
    fn the_toplevel_outlives_its_window_while_other_windows_show_on_it() {
        // A splash activity that finished while the main one, a subsurface of it, still shows.
        assert_eq!(on_close(true, 1, false), Close::Orphan);
        // The last window then closes the orphaned toplevel with it.
        assert_eq!(on_close(false, 0, true), Close::Destroy);
        // The toplevel's own window closing last closes it, as before.
        assert_eq!(on_close(true, 0, false), Close::Destroy);
        // A dialog closing leaves the toplevel alone, orphaned or not.
        assert_eq!(on_close(false, 1, false), Close::Keep);
        assert_eq!(on_close(false, 2, true), Close::Keep);
        assert_eq!(on_close(false, 0, false), Close::Keep);
    }
}
