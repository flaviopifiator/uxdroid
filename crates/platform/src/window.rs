//! A top-level Wayland window backed by shared memory.

use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::raster::Frame;

pub const RUNNING: i32 = 0;
pub const NEEDS_REDRAW: i32 = 1;
pub const CLOSED: i32 = 2;

const BYTES_PER_PIXEL: i32 = 4;
const MAX_DIMENSION: i32 = 16384;

#[derive(Default)]
struct State {
    configure_serial: Option<u32>,
    requested_size: Option<(i32, i32)>,
    closed: bool,
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
    queue: EventQueue<State>,
    state: State,
    surface: wl_surface::WlSurface,
    _xdg_surface: xdg_surface::XdgSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
    shm: wl_shm::WlShm,
    pixels: Pixels,
    width: i32,
    height: i32,
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

        let surface = compositor.create_surface(&handle, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &handle, ());
        let toplevel = xdg_surface.get_toplevel(&handle, ());
        toplevel.set_title(title.to_string());
        // The title is the app's package; the id is `uxdroid-<package>`, as `crates/system` gives it.
        toplevel.set_app_id(format!("uxdroid-{title}"));
        surface.commit();

        let mut state = State::default();
        while state.configure_serial.is_none() && !state.closed {
            queue.blocking_dispatch(&mut state)?;
        }
        let (width, height) = state.requested_size.take().unwrap_or((width, height));
        let pixels = allocate(&shm, &handle, width, height)?;
        Ok(Window {
            queue,
            state,
            surface,
            _xdg_surface: xdg_surface,
            _toplevel: toplevel,
            shm,
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

    pub fn wait_for_events(&mut self) -> Result<i32, Box<dyn Error>> {
        // `present` also dispatches events, so what it received must not wait for another event.
        if !self.state.closed && self.state.requested_size.is_none() {
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        if self.state.closed {
            return Ok(CLOSED);
        }
        if let Some((width, height)) = self.state.requested_size.take()
            && (width, height) != (self.width, self.height)
        {
            let handle = self.queue.handle();
            self.pixels = allocate(&self.shm, &handle, width, height)?;
            self.width = width;
            self.height = height;
            return Ok(NEEDS_REDRAW);
        }
        Ok(RUNNING)
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
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                // A compositor can ask for anything, and the window takes what it can hold.
                state.requested_size = Some((width.min(MAX_DIMENSION), height.min(MAX_DIMENSION)));
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
