//! The PulseAudio protocol through `libpulse-simple`, loaded when first needed: uxdroid does not
//! link the host's sound libraries, so it starts on hosts without them. PipeWire serves the same
//! protocol through `pipewire-pulse`.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::time::Duration;

use crate::backend::{Capture, Sink};
use crate::{Config, Error};

const PLAYBACK: c_int = 1;
const RECORD: c_int = 2;
const FLOAT32LE: c_int = 5;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

/// `pa_buffer_attr`; `u32::MAX` asks the server's default.
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

type New = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    c_int,
    *const c_char,
    *const c_char,
    *const SampleSpec,
    *const c_void,
    *const BufferAttr,
    *mut c_int,
) -> *mut c_void;
type Transfer = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int;
type Read = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type Latency = unsafe extern "C" fn(*mut c_void, *mut c_int) -> u64;
type Free = unsafe extern "C" fn(*mut c_void);
type Flush = unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int;
type StrError = unsafe extern "C" fn(c_int) -> *const c_char;

struct Library {
    new: New,
    write: Transfer,
    read: Read,
    latency: Latency,
    free: Free,
    flush: Flush,
    strerror: StrError,
}

fn library() -> Result<&'static Library, Error> {
    static LIBRARY: OnceLock<Result<Library, String>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let simple = crate::dl::open(c"libpulse-simple.so.0")?;
            let pulse = crate::dl::open(c"libpulse.so.0")?;
            // SAFETY: each symbol has the signature of libpulse's public headers.
            unsafe {
                Ok(Library {
                    new: crate::dl::function(simple, c"pa_simple_new")?,
                    write: crate::dl::function(simple, c"pa_simple_write")?,
                    read: crate::dl::function(simple, c"pa_simple_read")?,
                    latency: crate::dl::function(simple, c"pa_simple_get_latency")?,
                    free: crate::dl::function(simple, c"pa_simple_free")?,
                    flush: crate::dl::function(simple, c"pa_simple_flush")?,
                    strerror: crate::dl::function(pulse, c"pa_strerror")?,
                })
            }
        })
        .as_ref()
        .map_err(|error| Error::Unavailable(format!("pulse: {error}")))
}

struct Stream {
    library: &'static Library,
    handle: *mut c_void,
}

// SAFETY: a `pa_simple` is used by one thread at a time, the stream's own.
unsafe impl Send for Stream {}

impl Stream {
    fn open(server: Option<&str>, config: Config, direction: c_int) -> Result<Stream, Error> {
        let library = library()?;
        let failed = |what: String| Error::Unavailable(format!("pulse: {what}"));
        let server = server
            .map(CString::new)
            .transpose()
            .map_err(|_| failed("a server name holds a nul".into()))?;
        let spec = SampleSpec {
            format: FLOAT32LE,
            rate: config.rate,
            channels: config.channels as u8,
        };
        let frame = 4 * u32::from(config.channels);
        let period = config.period() * frame;
        // Two periods queued on the server: low latency, and room for one late wake-up.
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: 2 * period,
            prebuf: u32::MAX,
            minreq: period,
            fragsize: period,
        };
        let mut error = 0;
        let name = if direction == PLAYBACK {
            c"playback"
        } else {
            c"record"
        };
        // SAFETY: valid strings and structures, which libpulse copies.
        let handle = unsafe {
            (library.new)(
                server.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                c"uxdroid".as_ptr(),
                direction,
                std::ptr::null(),
                name.as_ptr(),
                &spec,
                std::ptr::null(),
                &attr,
                &mut error,
            )
        };
        if handle.is_null() {
            return Err(failed(library.message(error)));
        }
        Ok(Stream { library, handle })
    }
}

impl Library {
    fn message(&self, error: c_int) -> String {
        // SAFETY: pa_strerror returns a static string or null.
        let text = unsafe { (self.strerror)(error) };
        if text.is_null() {
            format!("error {error}")
        } else {
            // SAFETY: checked non-null; libpulse's strings are nul-terminated.
            unsafe { std::ffi::CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the handle came from pa_simple_new and is freed once.
        unsafe { (self.library.free)(self.handle) };
    }
}

impl Sink for Stream {
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let mut error = 0;
        // SAFETY: the slice is valid for its length in bytes.
        let result = unsafe {
            (self.library.write)(
                self.handle,
                samples.as_ptr().cast(),
                size_of_val(samples),
                &mut error,
            )
        };
        if result < 0 {
            return Err(format!("pulse: {}", self.library.message(error)));
        }
        Ok(())
    }

    fn latency(&self) -> Duration {
        let mut error = 0;
        // SAFETY: a live handle.
        let micros = unsafe { (self.library.latency)(self.handle, &mut error) };
        if micros == u64::MAX {
            Duration::ZERO
        } else {
            Duration::from_micros(micros)
        }
    }
}

impl Capture for Stream {
    /// The server records from the moment the stream connects; what it kept before the app started
    /// recording is stale, so it is dropped.
    fn resume(&mut self) -> Result<(), String> {
        let mut error = 0;
        // SAFETY: a live handle.
        if unsafe { (self.library.flush)(self.handle, &mut error) } < 0 {
            return Err(format!("pulse: {}", self.library.message(error)));
        }
        Ok(())
    }

    fn read(&mut self, samples: &mut [f32]) -> Result<(), String> {
        let mut error = 0;
        // SAFETY: the slice is valid and writable for its length in bytes.
        let result = unsafe {
            (self.library.read)(
                self.handle,
                samples.as_mut_ptr().cast(),
                size_of_val(samples),
                &mut error,
            )
        };
        if result < 0 {
            return Err(format!("pulse: {}", self.library.message(error)));
        }
        Ok(())
    }
}

pub(crate) fn sink(server: Option<&str>, config: Config) -> Result<Box<dyn Sink>, Error> {
    Ok(Box::new(Stream::open(server, config, PLAYBACK)?))
}

pub(crate) fn capture(server: Option<&str>, config: Config) -> Result<Box<dyn Capture>, Error> {
    Ok(Box::new(Stream::open(server, config, RECORD)?))
}
