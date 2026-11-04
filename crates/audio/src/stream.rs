//! Output and input streams, each with its device thread.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Capture, Sink};
use crate::{Config, Error, Flow, State};

/// Wakes a waiting app thread at least this often, so a closed stream never strands it.
const RECHECK: Duration = Duration::from_millis(100);

/// How long dropping a stream waits for a device that does not return, such as a sound server that
/// hung, before it lets the device thread end on its own. An app callback is always waited for.
const JOIN_LIMIT: Duration = Duration::from_secs(1);

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

/// When a wait of `timeout` ends; `None` waits forever, as does a timeout past the clock's range.
fn deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.and_then(|timeout| Instant::now().checked_add(timeout))
}

/// Time left until `deadline`: `Some(None)` without one, which never expires, and `None` once passed.
fn left(deadline: Option<Instant>) -> Option<Option<Duration>> {
    match deadline {
        None => Some(None),
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .map(Some),
    }
}

/// What a stream and its device thread tell each other about its end.
#[derive(Default)]
struct Life {
    /// The stream was dropped; the thread ends at its next look.
    closed: bool,
    /// The thread is inside the app's callback, which may use what the app frees after closing.
    busy: bool,
    /// The thread returned.
    ended: bool,
}

trait Lives {
    fn life(&mut self) -> &mut Life;
}

/// Marks the thread ended however it leaves, a panic included.
struct Ending<T: Lives>(Arc<Shared<T>>);

impl<T: Lives> Drop for Ending<T> {
    fn drop(&mut self) {
        self.0.lock().life().ended = true;
        self.0.changed.notify_all();
    }
}

fn spawn<T: Lives + Send + 'static>(
    name: &str,
    shared: &Arc<Shared<T>>,
    body: impl FnOnce(&Shared<T>) + Send + 'static,
) -> Result<JoinHandle<()>, Error> {
    let ending = Ending(shared.clone());
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || body(&ending.0))
        .map_err(|e| Error::Unavailable(format!("audio thread: {e}")))
}

/// Closes a stream: waits for its thread to leave the app's callback, then at most [`JOIN_LIMIT`]
/// for it to end, so a device that never returns does not hang the app.
fn close<T: Lives>(shared: &Shared<T>, thread: Option<JoinHandle<()>>, limit: Duration) {
    let mut inner = shared.lock();
    inner.life().closed = true;
    shared.changed.notify_all();
    let Some(thread) = thread else { return };
    // Dropped by the stream's own callback, through an app that let go of it there: the thread
    // sees `closed` and ends by itself.
    if thread.thread().id() == std::thread::current().id() {
        return;
    }
    let deadline = Instant::now() + limit;
    loop {
        let life = inner.life();
        if life.ended {
            drop(inner);
            let _ = thread.join();
            return;
        }
        let now = Instant::now();
        if !life.busy && now >= deadline {
            eprintln!("uxdroid: audio device did not return in time; its thread ends on its own");
            return;
        }
        let wait = if life.busy {
            RECHECK
        } else {
            (deadline - now).min(RECHECK)
        };
        inner = shared.wait(inner, wait);
    }
}

/// Runs the app's callback, marked busy so a close waits for it; `None` if it panicked.
fn call<'a, T: Lives, R>(
    shared: &'a Shared<T>,
    mut inner: MutexGuard<'a, T>,
    callback: impl FnOnce() -> R,
) -> (Option<R>, MutexGuard<'a, T>) {
    inner.life().busy = true;
    drop(inner);
    let result = std::panic::catch_unwind(AssertUnwindSafe(callback)).ok();
    let mut inner = shared.lock();
    inner.life().busy = false;
    shared.changed.notify_all();
    (result, inner)
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
    life: Life,
}

impl Lives for Playback {
    fn life(&mut self) -> &mut Life {
        &mut self.life
    }
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
        Output::on_sink(backend.sink(config)?, config, callback)
    }

    fn on_sink(
        sink: Box<dyn Sink>,
        config: Config,
        callback: Option<OutputCallback>,
    ) -> Result<Output, Error> {
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
            life: Life::default(),
        });
        let thread = spawn("uxdroid-audio-out", &shared, move |shared| {
            play(shared, config, sink, callback);
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
        self.write_within(samples, if blocking { None } else { Some(Duration::ZERO) })
    }

    /// As [`Output::write`], waiting at most `timeout` for room, or forever when `None`.
    pub fn write_within(&self, samples: &[f32], timeout: Option<Duration>) -> usize {
        let deadline = deadline(timeout);
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
            let left = left(deadline);
            if done == total || left.is_none() || inner.life.closed || failed {
                return done;
            }
            inner = self.shared.wait(
                inner,
                left.flatten().map_or(RECHECK, |left| left.min(RECHECK)),
            );
        }
    }

    /// Lets the device go, as AAudio's release does, while the stream is still held: its thread
    /// ends, and a write blocked on it returns, as does every later one, with nothing taken.
    pub fn release(&self) {
        close(&self.shared, None, JOIN_LIMIT);
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
        let deadline = deadline(Some(timeout));
        let mut inner = self.shared.lock();
        while inner.state == state {
            let Some(left) = left(deadline) else {
                break;
            };
            inner = self
                .shared
                .wait(inner, left.map_or(RECHECK, |left| left.min(RECHECK)));
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

    /// The frame the device presents now: the position less what the device holds. The position
    /// counts source frames, which the device plays at the playback rate.
    pub fn presented(&self) -> u64 {
        let inner = self.shared.lock();
        let held = inner.latency.as_secs_f64() * f64::from(inner.rate);
        inner.position.saturating_sub(held as u64)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        close(&self.shared, self.thread.take(), JOIN_LIMIT);
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
            if inner.life.closed {
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
            let (flow, mut after) = call(shared, inner, || callback(&mut buffer));
            let Some(flow) = flow else {
                eprintln!("uxdroid: audio output stopped: its callback panicked");
                after.state = State::Disconnected;
                return;
            };
            inner = after;
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
    callback: bool,
    life: Life,
}

impl Lives for Recording {
    fn life(&mut self) -> &mut Life {
        &mut self.life
    }
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
        Input::on_capture(backend.capture(config)?, config, callback)
    }

    fn on_capture(
        capture: Box<dyn Capture>,
        config: Config,
        callback: Option<InputCallback>,
    ) -> Result<Input, Error> {
        let shared = Shared::new(Recording {
            state: State::Stopped,
            ring: VecDeque::new(),
            capacity: config.capacity.max(config.period()) as usize * usize::from(config.channels),
            position: 0,
            overruns: 0,
            full: false,
            callback: callback.is_some(),
            life: Life::default(),
        });
        let thread = spawn("uxdroid-audio-in", &shared, move |shared| {
            record(shared, config, capture, callback);
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

    /// Lets the device go while the stream is still held: a read blocked on it returns, as does every
    /// later one, with nothing read.
    pub fn release(&self) {
        close(&self.shared, None, JOIN_LIMIT);
    }

    /// Reads whole frames into `out` and returns how many samples. A blocking read waits for them
    /// while the stream records; a callback stream yields nothing.
    pub fn read(&self, out: &mut [f32], blocking: bool) -> usize {
        self.read_within(out, if blocking { None } else { Some(Duration::ZERO) })
    }

    /// As [`Input::read`], waiting at most `timeout` for frames, or forever when `None`.
    pub fn read_within(&self, out: &mut [f32], timeout: Option<Duration>) -> usize {
        let deadline = deadline(timeout);
        let channels = usize::from(self.config.channels);
        let total = out.len() / channels * channels;
        let mut done = 0;
        let mut inner = self.shared.lock();
        if inner.callback {
            return 0;
        }
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
            let left = left(deadline);
            if done == total || left.is_none() || inner.life.closed || inner.state != State::Playing
            {
                return done;
            }
            inner = self.shared.wait(
                inner,
                left.flatten().map_or(RECHECK, |left| left.min(RECHECK)),
            );
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
        close(&self.shared, self.thread.take(), JOIN_LIMIT);
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
            if inner.life.closed {
                return;
            }
            if inner.state == State::Playing {
                break;
            }
            running = false;
            inner = shared.wait(inner, RECHECK);
        }
        drop(inner);
        let resumed = if running {
            Ok(())
        } else {
            running = true;
            capture.resume()
        };
        if let Err(error) = resumed.and_then(|()| capture.read(&mut buffer)) {
            eprintln!("uxdroid: audio input stopped: {error}");
            shared.lock().state = State::Disconnected;
            shared.changed.notify_all();
            return;
        }
        let mut inner = shared.lock();
        // A read can outlast close's wait: the app may have freed what its callback uses.
        if inner.life.closed {
            return;
        }
        if inner.state != State::Playing {
            continue;
        }
        inner.position += u64::from(config.period());
        if let Some(callback) = &mut callback {
            let (flow, mut inner) = call(shared, inner, || callback(&buffer));
            match flow {
                None => {
                    eprintln!("uxdroid: audio input stopped: its callback panicked");
                    inner.state = State::Disconnected;
                    return;
                }
                Some(Flow::Stop) if inner.state == State::Playing => inner.state = State::Stopped,
                Some(_) => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A device that takes frames as fast as they come and reports a fixed latency.
    struct Late(Duration);

    impl Sink for Late {
        fn write(&mut self, _: &[f32]) -> Result<(), String> {
            Ok(())
        }
        fn latency(&self) -> Duration {
            self.0
        }
    }

    /// A device whose first write never returns, as a hung sound server's.
    struct Hung(Option<mpsc::Sender<()>>);

    impl Sink for Hung {
        fn write(&mut self, _: &[f32]) -> Result<(), String> {
            if let Some(entered) = self.0.take() {
                let _ = entered.send(());
            }
            loop {
                std::thread::park();
            }
        }
    }

    fn config() -> Config {
        Config {
            rate: 8_000,
            channels: 1,
            capacity: 4_000,
        }
    }

    #[test]
    fn the_presented_frame_counts_held_frames_at_the_playback_rate() {
        let output =
            Output::on_sink(Box::new(Late(Duration::from_millis(100))), config(), None).unwrap();
        assert!(output.set_rate(16_000));
        output.write(&[0.5; 4_000], true);
        output.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        while output.buffered() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        output.pause();
        std::thread::sleep(Duration::from_millis(20));
        // 100 ms of device time hold 1,600 source frames at twice the device rate.
        assert_eq!(output.position(), 4_000);
        assert_eq!(output.presented(), 4_000 - 1_600);
    }

    #[test]
    fn releasing_a_stream_returns_a_write_blocked_on_it() {
        let output =
            Arc::new(Output::on_sink(Box::new(Late(Duration::ZERO)), config(), None).unwrap());
        // Full and never started: a blocking write waits for room that never comes.
        output.write(&[0.5; 4_000], true);
        let (done, written) = mpsc::channel();
        let writer = output.clone();
        std::thread::spawn(move || {
            let _ = done.send(writer.write(&[0.5; 80], true));
        });
        std::thread::sleep(Duration::from_millis(50));
        output.release();
        assert_eq!(written.recv_timeout(Duration::from_secs(1)), Ok(0));
        assert_eq!(
            output.write(&[0.5; 80], true),
            0,
            "a released stream takes nothing"
        );
    }

    #[test]
    fn dropping_a_stream_whose_device_hung_returns_within_the_limit() {
        let (entered, inside) = mpsc::channel();
        let output = Output::on_sink(Box::new(Hung(Some(entered))), config(), None).unwrap();
        output.write(&[0.5; 80], true);
        output.start();
        inside.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, dropped) = mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            drop(output);
            let _ = done.send(());
        });
        dropped.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() >= JOIN_LIMIT);
    }

    /// A device whose first read waits until the test lets it return, as a stalled sound server's.
    struct Stalled {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Capture for Stalled {
        fn read(&mut self, samples: &mut [f32]) -> Result<(), String> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                let _ = self.release.recv();
            }
            samples.fill(0.25);
            Ok(())
        }
    }

    #[test]
    fn an_input_callback_never_runs_after_its_stream_was_dropped() {
        let (entered, inside) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (called, calls) = mpsc::channel();
        let capture = Stalled {
            entered: Some(entered),
            release: released,
        };
        let callback: InputCallback = Box::new(move |_: &[f32]| {
            let _ = called.send(());
            Flow::Continue
        });
        let input = Input::on_capture(Box::new(capture), config(), Some(callback)).unwrap();
        input.start();
        inside.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(input);
        // The stalled read returns only after the stream is gone.
        release.send(()).unwrap();
        assert!(calls.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
