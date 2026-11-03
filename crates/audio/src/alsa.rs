//! ALSA through `libasound`, loaded when first needed: the fallback on hosts with no sound server.

use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::OnceLock;

use crate::backend::{Capture, Sink};
use crate::{Config, Error};

const PLAYBACK: c_int = 0;
const CAPTURE: c_int = 1;
const FLOAT_LE: c_int = 14;
const RW_INTERLEAVED: c_int = 3;

type Open = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
type SetParams =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
type Transfer = unsafe extern "C" fn(*mut c_void, *mut c_void, c_ulong) -> c_long;
type Recover = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type StrError = unsafe extern "C" fn(c_int) -> *const c_char;

struct Library {
    open: Open,
    set_params: SetParams,
    writei: Transfer,
    readi: Transfer,
    recover: Recover,
    close: Close,
    strerror: StrError,
}

fn library() -> Result<&'static Library, Error> {
    static LIBRARY: OnceLock<Result<Library, String>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let asound = crate::dl::open(c"libasound.so.2")?;
            // SAFETY: each symbol has the signature of alsa-lib's public headers.
            unsafe {
                Ok(Library {
                    open: crate::dl::function(asound, c"snd_pcm_open")?,
                    set_params: crate::dl::function(asound, c"snd_pcm_set_params")?,
                    writei: crate::dl::function(asound, c"snd_pcm_writei")?,
                    readi: crate::dl::function(asound, c"snd_pcm_readi")?,
                    recover: crate::dl::function(asound, c"snd_pcm_recover")?,
                    close: crate::dl::function(asound, c"snd_pcm_close")?,
                    strerror: crate::dl::function(asound, c"snd_strerror")?,
                })
            }
        })
        .as_ref()
        .map_err(|error| Error::Unavailable(format!("alsa: {error}")))
}

impl Library {
    fn message(&self, error: c_int) -> String {
        // SAFETY: snd_strerror returns a static string.
        let text = unsafe { (self.strerror)(error) };
        if text.is_null() {
            return format!("error {error}");
        }
        // SAFETY: checked non-null and nul-terminated.
        unsafe { std::ffi::CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

struct Pcm {
    library: &'static Library,
    handle: *mut c_void,
    channels: usize,
}

// SAFETY: a PCM is used by one thread at a time, the stream's own.
unsafe impl Send for Pcm {}

impl Pcm {
    fn open(device: &str, config: Config, direction: c_int) -> Result<Pcm, Error> {
        let library = library()?;
        let failed = |what: String| Error::Unavailable(format!("alsa {device}: {what}"));
        let name = CString::new(device).map_err(|_| failed("a nul in the name".into()))?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: valid output pointer and name.
        let result = unsafe { (library.open)(&mut handle, name.as_ptr(), direction, 0) };
        if result < 0 {
            return Err(failed(library.message(result)));
        }
        let pcm = Pcm {
            library,
            handle,
            channels: usize::from(config.channels),
        };
        // Two periods of latency, with ALSA's resampler when the device lacks the rate.
        let latency = 2 * config.period() * 1_000_000 / config.rate;
        // SAFETY: a PCM just opened.
        let result = unsafe {
            (library.set_params)(
                handle,
                FLOAT_LE,
                RW_INTERLEAVED,
                c_uint::from(config.channels),
                config.rate,
                1,
                latency,
            )
        };
        if result < 0 {
            return Err(failed(library.message(result)));
        }
        Ok(pcm)
    }

    fn transfer(&mut self, samples: *mut f32, count: usize, playing: bool) -> Result<(), String> {
        let mut done = 0;
        let frames = count / self.channels;
        while done < frames {
            // SAFETY: the buffer holds `frames` whole frames; `done` stays below.
            let result = unsafe {
                let at = samples.add(done * self.channels).cast();
                let left = (frames - done) as c_ulong;
                if playing {
                    (self.library.writei)(self.handle, at, left)
                } else {
                    (self.library.readi)(self.handle, at, left)
                }
            };
            if result < 0 {
                // An xrun or a suspend: restart the device and carry on, as aplay does.
                // SAFETY: a live PCM.
                let recovered = unsafe { (self.library.recover)(self.handle, result as c_int, 1) };
                if recovered < 0 {
                    return Err(format!("alsa: {}", self.library.message(recovered)));
                }
            } else {
                done += result as usize;
            }
        }
        Ok(())
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        // SAFETY: opened by snd_pcm_open and closed once.
        unsafe { (self.library.close)(self.handle) };
    }
}

impl Sink for Pcm {
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        // ALSA only reads from the buffer when playing.
        self.transfer(samples.as_ptr().cast_mut(), samples.len(), true)
    }
}

impl Capture for Pcm {
    fn read(&mut self, samples: &mut [f32]) -> Result<(), String> {
        self.transfer(samples.as_mut_ptr(), samples.len(), false)
    }
}

pub(crate) fn sink(device: &str, config: Config) -> Result<Box<dyn Sink>, Error> {
    Ok(Box::new(Pcm::open(device, config, PLAYBACK)?))
}

pub(crate) fn capture(device: &str, config: Config) -> Result<Box<dyn Capture>, Error> {
    Ok(Box::new(Pcm::open(device, config, CAPTURE)?))
}
