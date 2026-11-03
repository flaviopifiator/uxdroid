//! Audio streams for Android apps, played and recorded through the host.
//!
//! Every Android audio API an app uses, Java's `AudioTrack` and `AudioRecord`, OpenSL ES and AAudio,
//! becomes one [`Output`] or [`Input`] here. A stream carries interleaved `f32` frames at its own
//! rate and channel count; each API converts its app's [`Format`] at its edge. Each stream has one
//! thread that moves a period (10 ms) at a time between the stream and its [`Backend`]'s device,
//! whose clock paces it. A push stream buffers what the app writes; a callback stream asks the app
//! for each period on that thread, as AAudio and OpenSL ES buffer queues expect.
//!
//! The backend is the PulseAudio protocol, which PipeWire also serves, then ALSA, through the host's
//! own libraries loaded at run time; or a file per stream, which tests read back sample by sample.

mod alsa;
mod backend;
mod dl;
mod format;
mod pulse;
mod stream;

use std::fmt;

pub use backend::Backend;
pub use format::Format;
pub use stream::{Input, Output};

/// Android's limits, `AudioFormat.SAMPLE_RATE_HZ_MIN` and `SAMPLE_RATE_HZ_MAX`.
pub const MIN_RATE: u32 = 4_000;
pub const MAX_RATE: u32 = 192_000;
/// `FCC_8`, the most channels an Android track mixes.
pub const MAX_CHANNELS: u16 = 8;

/// A stream's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Frames per second.
    pub rate: u32,
    pub channels: u16,
    /// Frames a push stream buffers between the app and the device.
    pub capacity: u32,
}

impl Config {
    /// Frames moved at each device tick: 10 ms, as Android's mixer period.
    pub fn period(&self) -> u32 {
        (self.rate / 100).max(1)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if !(MIN_RATE..=MAX_RATE).contains(&self.rate) {
            return Err(Error::Invalid(format!(
                "sample rate {} outside {MIN_RATE}..={MAX_RATE}",
                self.rate
            )));
        }
        if !(1..=MAX_CHANNELS).contains(&self.channels) {
            return Err(Error::Invalid(format!(
                "{} channels outside 1..={MAX_CHANNELS}",
                self.channels
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The app asked for something Android refuses too.
    Invalid(String),
    /// The host's device or sound server could not be opened.
    Unavailable(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(text) | Error::Unavailable(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Error {}

/// What a callback asks of its stream after it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Stop the stream after this period, as `AAUDIO_CALLBACK_RESULT_STOP` does.
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Playing, or recording.
    Playing,
    Paused,
    /// Stopped by the app, playing what it buffered before it stops.
    Draining,
    /// The device failed; the stream moves nothing more.
    Disconnected,
}
