//! A Wayland toplevel for a framework window: shared-memory pixels, as milestone 3's window in
//! `crates/platform`, plus the seat, whose pointer, keyboard and touch events become the window's
//! input, and reading without blocking, so the Looper can watch the connection while the window is
//! idle and answer the compositor's pings.

use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::FileExt;

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface, wl_touch,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::input::charmap::{self, Charmap};
use crate::input::keys::{self, Modifiers};
use crate::input::translate::Translator;
use crate::input::xkb::Keymap;
use crate::input::{self, Event};

const BYTES_PER_PIXEL: i32 = 4;
const MAX_DIMENSION: i32 = 16384;
/// A keymap is a few tens of kilobytes; anything far larger is refused.
const MAX_KEYMAP: u32 = 4 << 20;
/// `xdg_toplevel.state.activated`.
const ACTIVATED: u32 = 4;

#[derive(Default)]
struct State {
    configure_serial: Option<u32>,
    requested_size: Option<(i32, i32)>,
    closed: bool,
    activated: bool,
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
    events: Vec<Event>,
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

pub struct Window {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    surface: wl_surface::WlSurface,
    _xdg_surface: xdg_surface::XdgSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
    _seat: Option<wl_seat::WlSeat>,
    _shm: wl_shm::WlShm,
    pixels: Pixels,
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
    pub fn open(title: &str, width: i32, height: i32) -> Result<Window, Box<dyn Error>> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue): (GlobalList, EventQueue<State>) =
            registry_queue_init(&connection)?;
        let handle = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&handle, 4..=6, ())?;
        let shm: wl_shm::WlShm = globals.bind(&handle, 1..=1, ())?;
        let shell: xdg_wm_base::XdgWmBase = globals.bind(&handle, 1..=6, ())?;
        // A compositor without a seat shows the window all the same, with no input.
        let seat: Option<wl_seat::WlSeat> = globals.bind(&handle, 1..=7, ()).ok();

        let surface = compositor.create_surface(&handle, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &handle, ());
        let toplevel = xdg_surface.get_toplevel(&handle, ());
        toplevel.set_title(title.to_string());
        toplevel.set_app_id(title.to_string());
        surface.commit();

        let mut state = State::default();
        while state.configure_serial.is_none() && !state.closed {
            queue.blocking_dispatch(&mut state)?;
        }
        let (width, height) = state.requested_size.take().unwrap_or((width, height));
        let pixels = allocate(&shm, &handle, width, height)?;
        Ok(Window {
            connection,
            queue,
            state,
            surface,
            _xdg_surface: xdg_surface,
            _toplevel: toplevel,
            _seat: seat,
            _shm: shm,
            pixels,
            width,
            height,
        })
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn frame(&mut self) -> Frame<'_> {
        // SAFETY: the mapping holds `length` bytes aligned to a page, and lives as long as `self`.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                self.pixels.mapping.cast::<u32>(),
                self.pixels.length / 4,
            )
        };
        Frame {
            pixels,
            width: self.width,
            height: self.height,
        }
    }

    pub fn present(&mut self) -> Result<(), Box<dyn Error>> {
        self.surface.attach(Some(&self.pixels.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, self.width, self.height);
        self.surface.commit();
        self.queue.flush()?;
        // A round trip proves the compositor took the frame before the caller goes on.
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    /// The connection's descriptor, readable when the compositor sent something.
    pub fn fd(&self) -> RawFd {
        self.connection.backend().poll_fd().as_raw_fd()
    }

    /// Reads and handles whatever the compositor sent, without blocking.
    pub fn dispatch(&mut self) -> Result<(), Box<dyn Error>> {
        self.queue.dispatch_pending(&mut self.state)?;
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        self.queue.flush()?;
        Ok(())
    }

    /// Input that arrived since the last call, in order.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.state.events)
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
                        state.events.extend(released);
                    }
                    state.events.push(Event::Focus(activated));
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
        let events = match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => state
                .seat
                .pointer_enter(surface_x as f32, surface_y as f32, now),
            wl_pointer::Event::Leave { .. } => state.seat.pointer_leave(now),
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
        state.events.extend(events);
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
                state.events.extend(released);
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
                state.events.extend(events);
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
        let events = match event {
            wl_touch::Event::Down { id, x, y, .. } => {
                state.seat.touch_down(id, x as f32, y as f32, now)
            }
            wl_touch::Event::Up { id, .. } => state.seat.touch_up(id, now),
            wl_touch::Event::Motion { id, x, y, .. } => {
                state.seat.touch_motion(id, x as f32, y as f32);
                Vec::new()
            }
            wl_touch::Event::Frame => state.seat.touch_frame(now),
            wl_touch::Event::Cancel => state.seat.touch_cancel(now),
            _ => Vec::new(),
        };
        state.events.extend(events);
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
