//! Output and input streams, each with its device thread.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Capture, Sink};
use crate::{Config, Error, Flow, State};

/// Wakes a waiting app thread at least this often, so a closed stream never strands it.
const RECHECK: Duration = Duration::from_millis(100);

struct Shared<T> {
    inner: Mutex<T>,
    changed: Condvar,
}

impl<T> Shared<T> {
    fn new(inner: T) -> Arc<Self> {
        Arc::new(Shared {
            inner: Mutex::new(inner),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, T> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, T>, timeout: Duration) -> MutexGuard<'a, T> {
        self.changed
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|p| p.into_inner())
            .0
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, Error> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map_err(|e| Error::Unavailable(format!("audio thread: {e}")))
}

struct Playback {
    state: State,
    ring: VecDeque<f32>,
    capacity: usize,
    callback: bool,
    /// Fractional read position into `ring`, in source frames, for resampling.
    cursor: f64,
    /// Source frames played.
    position: u64,
    written: u64,
    played: bool,
    starving: bool,
    underruns: u32,
    volume: [f32; 2],
    rate: u32,
    latency: Duration,
    closed: bool,
}

/// Frames to the device, written by the app or asked from its callback.
pub struct Output {
    shared: Arc<Shared<Playback>>,
    config: Config,
    thread: Option<JoinHandle<()>>,
}

type OutputCallback = Box<dyn FnMut(&mut [f32]) -> Flow + Send>;

impl Output {
    /// A push stream, which buffers `config.capacity` frames.
    pub fn open(backend: &Backend, config: Config) -> Result<Output, Error> {
        Output::start_thread(backend, config, None)
    }

    /// A callback stream: `callback` fills each period, on the stream's thread, while it plays.
    pub fn with_callback(
        backend: &Backend,
        config: Config,
        callback: impl FnMut(&mut [f32]) -> Flow + Send + 'static,
    ) -> Result<Output, Error> {
        Output::start_thread(backend, config, Some(Box::new(callback)))
    }

    fn start_thread(
        backend: &Backend,
        config: Config,
        callback: Option<OutputCallback>,
    ) -> Result<Output, Error> {
        config.validate()?;
        let sink = backend.sink(config)?;
        let channels = usize::from(config.channels);
        let shared = Shared::new(Playback {
            state: State::Stopped,
            ring: VecDeque::new(),
            capacity: config.capacity.max(config.period()) as usize * channels,
            callback: callback.is_some(),
            cursor: 0.0,
            position: 0,
            written: 0,
            played: false,
            starving: false,
            underruns: 0,
            volume: [1.0, 1.0],
            rate: config.rate,
            latency: Duration::ZERO,
            closed: false,
        });
        let thread_shared = shared.clone();
        let thread = spawn("uxdroid-audio-out", move || {
            play(&thread_shared, config, sink, callback);
        })?;
        Ok(Output {
            shared,
            config,
            thread: Some(thread),
        })
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// Buffers whole frames of `samples` and returns how many samples it took. A blocking write
    /// waits for room while the stream plays; a callback stream takes nothing.
    pub fn write(&self, samples: &[f32], blocking: bool) -> usize {
        let channels = usize::from(self.config.channels);
        let total = samples.len() / channels * channels;
        let mut done = 0;
        let mut inner = self.shared.lock();
        if inner.callback {
            return 0;
        }
        loop {
            let room =
                (inner.capacity - inner.ring.len().min(inner.capacity)) / channels * channels;
            let take = room.min(total - done);
            inner.ring.extend(&samples[done..done + take]);
            inner.written += (take / channels) as u64;
            done += take;
            // A stopped stream blocks too, as on Android: it waits for the app to play.
            let failed = inner.state == State::Disconnected;
            if done == total || !blocking || inner.closed || failed {
                return done;
            }
            inner = self.shared.wait(inner, RECHECK);
        }
    }

    /// Plays from the buffer or the callback; false once the device has failed.
    pub fn start(&self) -> bool {
        let mut inner = self.shared.lock();
        if inner.state == State::Disconnected {
            return false;
        }
        inner.state = State::Playing;
        inner.played = false;
        inner.starving = false;
        self.shared.changed.notify_all();
        true
    }

    pub fn pause(&self) {
        self.set(|state| {
            matches!(state, State::Playing | State::Draining).then_some(State::Paused)
        });
    }

    /// Stops a callback stream at once, and a push stream once it played what it buffered.
    pub fn stop(&self) {
        let mut inner = self.shared.lock();
        inner.state = match inner.state {
            State::Disconnected => State::Disconnected,
            State::Playing if !inner.callback && !inner.ring.is_empty() => State::Draining,
            _ => State::Stopped,
        };
        self.shared.changed.notify_all();
    }

    /// Drops what is buffered, unless the stream is playing.
    pub fn flush(&self) {
        let mut inner = self.shared.lock();
        if !matches!(inner.state, State::Playing | State::Draining) {
            inner.ring.clear();
            inner.cursor = 0.0;
        }
        self.shared.changed.notify_all();
    }

    fn set(&self, change: impl FnOnce(State) -> Option<State>) {
        let mut inner = self.shared.lock();
        if let Some(state) = change(inner.state) {
            inner.state = state;
            self.shared.changed.notify_all();
        }
    }

    pub fn state(&self) -> State {
        self.shared.lock().state
    }

    /// Waits up to `timeout` for the state to leave `state`, and returns the state then.
    pub fn wait_while(&self, state: State, timeout: Duration) -> State {
        let deadline = Instant::now() + timeout;
        let mut inner = self.shared.lock();
        while inner.state == state {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            inner = self.shared.wait(inner, left.min(RECHECK));
        }
        inner.state
    }

    /// Source frames handed to the device since the stream opened.
    pub fn position(&self) -> u64 {
        self.shared.lock().position
    }

    /// Frames the app wrote since the stream opened.
    pub fn written(&self) -> u64 {
        self.shared.lock().written
    }

    /// Times the buffer ran dry while playing, after the stream had started to play.
    pub fn underruns(&self) -> u32 {
        self.shared.lock().underruns
    }

    /// Frames that fit in the buffer now.
    pub fn available(&self) -> usize {
        let inner = self.shared.lock();
        (inner.capacity - inner.ring.len().min(inner.capacity)) / usize::from(self.config.channels)
    }

    /// Frames buffered and not yet handed to the device.
    pub fn buffered(&self) -> usize {
        self.shared.lock().ring.len() / usize::from(self.config.channels)
    }

    /// The gains of the first and second channels; other channels take the first's.
    pub fn set_volume(&self, left: f32, right: f32) {
        self.shared.lock().volume = [left.clamp(0.0, 1.0), right.clamp(0.0, 1.0)];
    }

    /// Plays the buffered frames as if they were recorded at `rate`, resampled to the stream's own
    /// rate, as `AudioTrack.setPlaybackRate` does. False when outside Android's limits.
    pub fn set_rate(&self, rate: u32) -> bool {
        if !(crate::MIN_RATE..=crate::MAX_RATE).contains(&rate) {
            return false;
        }
        self.shared.lock().rate = rate;
        true
    }

    pub fn rate(&self) -> u32 {
        self.shared.lock().rate
    }

    /// How long a frame written now waits before the device plays it.
    pub fn latency(&self) -> Duration {
        let inner = self.shared.lock();
        let frames = (inner.ring.len() / usize::from(self.config.channels)) as u64;
        inner.latency + Duration::from_nanos(frames * 1_000_000_000 / u64::from(inner.rate))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn play(
    shared: &Shared<Playback>,
    config: Config,
    mut sink: Box<dyn Sink>,
    mut callback: Option<OutputCallback>,
) {
    let channels = usize::from(config.channels);
    let mut buffer = vec![0f32; config.period() as usize * channels];
    let mut running = false;
    loop {
        let mut inner = shared.lock();
        loop {
            if inner.closed {
                return;
            }
            if matches!(inner.state, State::Playing | State::Draining) {
                break;
            }
            running = false;
            inner = shared.wait(inner, RECHECK);
        }
        if !running {
            sink.resume();
            running = true;
        }
        if let Some(callback) = &mut callback {
            drop(inner);
            let flow = callback(&mut buffer);
            inner = shared.lock();
            inner.position += config.period() as u64;
            if flow == Flow::Stop && inner.state == State::Playing {
                inner.state = State::Stopped;
            }
        } else {
            inner.render(&mut buffer, channels, config.rate);
        }
        let volume = inner.volume;
        shared.changed.notify_all();
        drop(inner);
        if volume != [1.0, 1.0] {
            for frame in buffer.chunks_exact_mut(channels) {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample *= volume[usize::from(channel == 1)];
                }
            }
        }
        if let Err(error) = sink.write(&buffer) {
            eprintln!("uxdroid: audio output stopped: {error}");
            shared.lock().state = State::Disconnected;
            shared.changed.notify_all();
            return;
        }
        let latency = sink.latency();
        shared.lock().latency = latency;
    }
}

impl Playback {
    /// Fills `out` from the buffer, resampling from the playback rate to `device_rate`, and accounts
    /// for draining and underruns.
    fn render(&mut self, out: &mut [f32], channels: usize, device_rate: u32) {
        let ratio = f64::from(self.rate) / f64::from(device_rate);
        let wanted = out.len() / channels;
        let mut produced = 0;
        while produced < wanted {
            let available = self.ring.len() / channels;
            let index = self.cursor as usize;
            let fraction = self.cursor - index as f64;
            if available < index + 1 + usize::from(fraction > 0.0) {
                break;
            }
            for channel in 0..channels {
                let first = self.ring[index * channels + channel];
                out[produced * channels + channel] = if fraction > 0.0 {
                    let second = self.ring[(index + 1) * channels + channel];
                    first + (second - first) * fraction as f32
                } else {
                    first
                };
            }
            produced += 1;
            self.cursor += ratio;
            let consumed = (self.cursor as usize).min(available);
            self.ring.drain(..consumed * channels);
            self.cursor -= consumed as f64;
            self.position += consumed as u64;
        }
        out[produced * channels..].fill(0.0);
        let short = produced < wanted;
        if produced > 0 {
            self.played = true;
        }
        if self.state == State::Draining {
            if short {
                self.position += (self.ring.len() / channels) as u64;
                self.ring.clear();
                self.cursor = 0.0;
                self.state = State::Stopped;
            }
        } else if short && self.played && !self.starving {
            self.underruns += 1;
        }
        self.starving = short;
    }
}

struct Recording {
    state: State,
    ring: VecDeque<f32>,
    capacity: usize,
    position: u64,
    overruns: u32,
    full: bool,
    closed: bool,
}

/// Frames from the device, read by the app or handed to its callback.
pub struct Input {
    shared: Arc<Shared<Recording>>,
    config: Config,
    thread: Option<JoinHandle<()>>,
}

type InputCallback = Box<dyn FnMut(&[f32]) -> Flow + Send>;

impl Input {
    /// A pull stream, which buffers `config.capacity` frames until the app reads them.
    pub fn open(backend: &Backend, config: Config) -> Result<Input, Error> {
        Input::start_thread(backend, config, None)
    }

    /// A callback stream: `callback` receives each period, on the stream's thread.
    pub fn with_callback(
        backend: &Backend,
        config: Config,
        callback: impl FnMut(&[f32]) -> Flow + Send + 'static,
    ) -> Result<Input, Error> {
        Input::start_thread(backend, config, Some(Box::new(callback)))
    }

    fn start_thread(
        backend: &Backend,
        config: Config,
        callback: Option<InputCallback>,
    ) -> Result<Input, Error> {
        config.validate()?;
        let capture = backend.capture(config)?;
        let shared = Shared::new(Recording {
            state: State::Stopped,
            ring: VecDeque::new(),
            capacity: config.capacity.max(config.period()) as usize * usize::from(config.channels),
            position: 0,
            overruns: 0,
            full: false,
            closed: false,
        });
        let thread_shared = shared.clone();
        let thread = spawn("uxdroid-audio-in", move || {
            record(&thread_shared, config, capture, callback);
        })?;
        Ok(Input {
            shared,
            config,
            thread: Some(thread),
        })
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn start(&self) -> bool {
        let mut inner = self.shared.lock();
        if inner.state == State::Disconnected {
            return false;
        }
        inner.state = State::Playing;
        self.shared.changed.notify_all();
        true
    }

    pub fn stop(&self) {
        let mut inner = self.shared.lock();
        if inner.state != State::Disconnected {
            inner.state = State::Stopped;
        }
        self.shared.changed.notify_all();
    }

    pub fn state(&self) -> State {
        self.shared.lock().state
    }

    /// Reads whole frames into `out` and returns how many samples. A blocking read waits for them
    /// while the stream records; a callback stream yields nothing.
    pub fn read(&self, out: &mut [f32], blocking: bool) -> usize {
        let channels = usize::from(self.config.channels);
        let total = out.len() / channels * channels;
        let mut done = 0;
        let mut inner = self.shared.lock();
        loop {
            let take = inner.ring.len().min(total - done);
            for (slot, sample) in out[done..done + take]
                .iter_mut()
                .zip(inner.ring.drain(..take))
            {
                *slot = sample;
            }
            done += take;
            if take > 0 {
                inner.full = false;
            }
            if done == total || !blocking || inner.closed || inner.state != State::Playing {
                return done;
            }
            inner = self.shared.wait(inner, RECHECK);
        }
    }

    /// Frames captured from the device since the stream opened.
    pub fn position(&self) -> u64 {
        self.shared.lock().position
    }

    /// Times captured frames were dropped because the app did not read in time.
    pub fn overruns(&self) -> u32 {
        self.shared.lock().overruns
    }

    /// Frames captured and not yet read.
    pub fn buffered(&self) -> usize {
        self.shared.lock().ring.len() / usize::from(self.config.channels)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn record(
    shared: &Shared<Recording>,
    config: Config,
    mut capture: Box<dyn Capture>,
    mut callback: Option<InputCallback>,
) {
    let mut buffer = vec![0f32; config.period() as usize * usize::from(config.channels)];
    let mut running = false;
    loop {
        let mut inner = shared.lock();
        loop {
            if inner.closed {
                return;
            }
            if inner.state == State::Playing {
                break;
            }
            running = false;
            inner = shared.wait(inner, RECHECK);
        }
        drop(inner);
        if !running {
            capture.resume();
            running = true;
        }
        if let Err(error) = capture.read(&mut buffer) {
            eprintln!("uxdroid: audio input stopped: {error}");
            shared.lock().state = State::Disconnected;
            shared.changed.notify_all();
            return;
        }
        let mut inner = shared.lock();
        if inner.state != State::Playing {
            continue;
        }
        inner.position += u64::from(config.period());
        if let Some(callback) = &mut callback {
            drop(inner);
            if callback(&buffer) == Flow::Stop {
                let mut inner = shared.lock();
                if inner.state == State::Playing {
                    inner.state = State::Stopped;
                }
            }
        } else {
            let room = inner.capacity - inner.ring.len().min(inner.capacity);
            let take = room.min(buffer.len());
            inner.ring.extend(&buffer[..take]);
            if take < buffer.len() && !inner.full {
                inner.overruns += 1;
            }
            inner.full = take < buffer.len();
        }
        shared.changed.notify_all();
    }
}
