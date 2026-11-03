//! Where a stream's samples go: the host sound server, or a sink tests can read.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::{Config, Error};

/// Plays interleaved `f32` frames at the stream's rate. `write` returns once the device can take
/// the next period, which is what paces a stream.
pub(crate) trait Sink: Send {
    fn write(&mut self, samples: &[f32]) -> Result<(), String>;
    /// Called when the stream plays again after a pause: a clock restarts from now.
    fn resume(&mut self) {}
    /// What the device holds and has not played yet.
    fn latency(&self) -> Duration {
        Duration::ZERO
    }
}

/// Records interleaved `f32` frames; `read` returns once a period was captured.
pub(crate) trait Capture: Send {
    fn read(&mut self, samples: &mut [f32]) -> Result<(), String>;
    fn resume(&mut self) {}
}

/// A host audio backend, chosen with `UXDROID_AUDIO`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The PulseAudio protocol, then ALSA's default device, then `Null`; the first that opens.
    Auto,
    /// Plays and records silence at the device rate.
    Null,
    /// Each output stream writes `output-<n>-<rate>hz-<channels>ch.f32` into the directory, and
    /// input streams read `input.f32` from it, then silence. Raw little-endian `f32`, interleaved.
    File(PathBuf),
    /// The PulseAudio protocol, which PipeWire's `pipewire-pulse` also serves; an optional server.
    Pulse(Option<String>),
    /// An ALSA device by name.
    Alsa(String),
}

impl Backend {
    /// `""` (auto), `null`, `file:<dir>`, `pulse`, `pulse:<server>`, `alsa` or `alsa:<device>`.
    pub fn parse(text: &str) -> Result<Backend, Error> {
        let (name, argument) = match text.split_once(':') {
            Some((name, argument)) => (name, Some(argument)),
            None => (text, None),
        };
        Ok(match (name, argument) {
            ("", None) | ("auto", None) => Backend::Auto,
            ("null", None) => Backend::Null,
            ("file", Some(dir)) if !dir.is_empty() => Backend::File(dir.into()),
            ("pulse", server) => Backend::Pulse(server.map(String::from)),
            ("alsa", None) => Backend::Alsa("default".into()),
            ("alsa", Some(device)) => Backend::Alsa(device.into()),
            _ => {
                return Err(Error::Invalid(format!(
                    "UXDROID_AUDIO={text:?}: expected null, file:<dir>, pulse[:<server>] or \
                     alsa[:<device>]"
                )));
            }
        })
    }

    /// The process' backend, from `UXDROID_AUDIO`, read once. An invalid value stops the process,
    /// so a typo never plays into the wrong place.
    pub fn current() -> &'static Backend {
        static CURRENT: OnceLock<Backend> = OnceLock::new();
        CURRENT.get_or_init(|| {
            let text = std::env::var("UXDROID_AUDIO").unwrap_or_default();
            Backend::parse(&text).unwrap_or_else(|error| {
                eprintln!("uxdroid: {error}");
                std::process::exit(1);
            })
        })
    }

    pub(crate) fn sink(&self, config: Config) -> Result<Box<dyn Sink>, Error> {
        match self {
            Backend::Auto => auto(|backend| backend.sink(config)),
            Backend::Null => Ok(Box::new(Clocked::new(config, Discard))),
            Backend::File(dir) => Ok(Box::new(Clocked::new(config, create_output(dir, config)?))),
            Backend::Pulse(server) => crate::pulse::sink(server.as_deref(), config),
            Backend::Alsa(device) => crate::alsa::sink(device, config),
        }
    }

    pub(crate) fn capture(&self, config: Config) -> Result<Box<dyn Capture>, Error> {
        match self {
            Backend::Auto => auto(|backend| backend.capture(config)),
            Backend::Null => Ok(Box::new(Clocked::new(config, Silence(None)))),
            Backend::File(dir) => {
                let file = File::open(dir.join("input.f32")).ok();
                Ok(Box::new(Clocked::new(config, Silence(file))))
            }
            Backend::Pulse(server) => crate::pulse::capture(server.as_deref(), config),
            Backend::Alsa(device) => crate::alsa::capture(device, config),
        }
    }
}

/// Tries the host's backends in order and remembers the first that opened, so later streams do
/// not probe again.
fn auto<T>(open: impl Fn(&Backend) -> Result<T, Error>) -> Result<T, Error> {
    static CHOSEN: OnceLock<Backend> = OnceLock::new();
    if let Some(backend) = CHOSEN.get() {
        return open(backend);
    }
    let mut reasons = Vec::new();
    for backend in [Backend::Pulse(None), Backend::Alsa("default".into())] {
        match open(&backend) {
            Ok(opened) => {
                let _ = CHOSEN.set(backend);
                return Ok(opened);
            }
            Err(error) => reasons.push(error.to_string()),
        }
    }
    eprintln!(
        "uxdroid: no sound server or device, audio plays into silence: {}",
        reasons.join("; ")
    );
    let _ = CHOSEN.set(Backend::Null);
    open(&Backend::Null)
}

fn create_output(dir: &std::path::Path, config: Config) -> Result<File, Error> {
    // Several libraries in one process each count streams, so the file system picks the number.
    for number in 0..10_000 {
        let path = dir.join(format!(
            "output-{number}-{}hz-{}ch.f32",
            config.rate, config.channels
        ));
        match File::options().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(Error::Unavailable(format!("{}: {error}", path.display())));
            }
        }
    }
    Err(Error::Unavailable(format!(
        "{}: too many outputs",
        dir.display()
    )))
}

/// A device without a clock of its own, paced by the monotonic clock at the stream's rate.
struct Clocked<T> {
    device: T,
    rate: u32,
    channels: usize,
    since: Instant,
    frames: u64,
}

impl<T> Clocked<T> {
    fn new(config: Config, device: T) -> Self {
        Clocked {
            device,
            rate: config.rate,
            channels: usize::from(config.channels),
            since: Instant::now(),
            frames: 0,
        }
    }

    /// Waits until the frames handed over so far would have played.
    fn pace(&mut self, samples: usize) {
        self.frames += (samples / self.channels) as u64;
        let due =
            self.since + Duration::from_nanos(self.frames * 1_000_000_000 / u64::from(self.rate));
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }

    fn restart(&mut self) {
        self.since = Instant::now();
        self.frames = 0;
    }
}

struct Discard;

impl Write for Discard {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<T: Write + Send> Sink for Clocked<T> {
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.device.write_all(&bytes).map_err(|e| e.to_string())?;
        self.pace(samples.len());
        Ok(())
    }
    fn resume(&mut self) {
        self.restart();
    }
}

/// Reads a file's samples, then silence.
struct Silence(Option<File>);

impl Capture for Clocked<Silence> {
    fn read(&mut self, samples: &mut [f32]) -> Result<(), String> {
        let mut bytes = vec![0u8; samples.len() * 4];
        let mut filled = 0;
        if let Some(file) = &mut self.device.0 {
            while filled < bytes.len() {
                match file.read(&mut bytes[filled..]) {
                    Ok(0) => break,
                    Ok(read) => filled += read,
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        bytes[filled - filled % 4..].fill(0);
        crate::Format::F32.decode(&bytes, samples);
        self.pace(samples.len());
        Ok(())
    }
    fn resume(&mut self) {
        self.restart();
    }
}
