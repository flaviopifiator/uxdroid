//! The Wayland side of an app's windows: one connection per process, whose one toplevel is the
//! display. The first window is the toplevel's surface; later ones, such as dialogs, popups and
//! menus, are subsurfaces of it, placed where the window manager put them, so the compositor
//! composes them and their frames stay apart. Pixels are shared memory, as milestone 3's window in
//! `crates/platform`. The seat's pointer, keyboard and touch events become input, tagged with the
//! surface they came from, and the connection is read without blocking, so the Looper can watch it
//! while the windows are idle and answer the compositor's pings. A configure with a new size and a
//! close are kept for the window manager to act on.

use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::FileExt;
use std::sync::{Mutex, MutexGuard};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface, wl_touch,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
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
    frames: Vec<u32>,
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
                eprintln!("uxdroid: the compositor's keymap does not compile; keys type nothing")
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
        Ok(Display {
            connection,
            queue,
            state: State::default(),
            compositor,
            subcompositor,
            shm,
            shell,
            _seat: seat,
            toplevel: None,
            size: (0, 0),
            windows: 0,
            orphan: None,
        })
    }
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
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wl_shm::Format::Xrgb8888,
        queue,
        (),
    );
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
    pub fn open(
        title: &str,
        width: i32,
        height: i32,
        x: i32,
        y: i32,
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
                let pixels = allocate(&display.shm, &handle, width, height)?;
                display.windows += 1;
                return Ok(Window {
                    surface,
                    role: Role::Sub(subsurface),
                    pixels: Some(pixels),
                    width,
                    height,
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
            let pixels = allocate(&display.shm, &handle, width, height)?;
            display.toplevel = Some(surface.clone());
            display.size = (width, height);
            display.windows += 1;
            Ok(Window {
                surface,
                role: Role::Toplevel(xdg_surface, toplevel),
                pixels: Some(pixels),
                width,
                height,
            })
        })
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
            let pixels = allocate(&display.shm, &handle, width, height)?;
            if matches!(self.role, Role::Toplevel(..)) {
                display.size = (width, height);
            }
            Ok(pixels)
        })?;
        self.pixels = Some(pixels);
        (self.width, self.height) = (width, height);
        Ok(())
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
            if !display.state.frames.contains(&key) {
                self.surface.frame(&display.queue.handle(), key);
                display.state.frames.push(key);
                vsync::frame_requested();
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
}

impl Drop for Window {
    fn drop(&mut self) {
        let mut guard = display();
        let Some(display) = guard.as_mut() else {
            return;
        };
        let key = self.key();
        if let Some(at) = display.state.frames.iter().position(|surface| *surface == key) {
            display.state.frames.remove(at);
            vsync::frame_dropped();
        }
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

impl Dispatch<wl_callback::WlCallback, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        surface: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(at) = state.frames.iter().position(|key| key == surface)
        {
            state.frames.remove(at);
            vsync::frame_done();
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
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
