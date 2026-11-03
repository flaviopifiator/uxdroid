//! `libaaudio`: AAudio streams over `uxdroid_audio`, the API Android recommends for native audio and
//! the one oboe opens first, through `dlopen`.
//!
//! A builder and a stream are objects kept by address in [`Live`], so a stale or forged pointer is
//! refused. A data callback runs on the stream's device thread, once per period; when an app asks
//! for another callback size, an adapter calls it as often as one period needs, as Android's does.
//! An input stream needs the manifest's `RECORD_AUDIO`, as on Android.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use uxdroid_audio::{Backend, Config, Flow, Format, Input, Output, State};
use uxdroid_bridge::table::{Export, Table};

use crate::live::Live;

const OK: i32 = 0;
const ERROR_DISCONNECTED: i32 = -899;
const ERROR_ILLEGAL_ARGUMENT: i32 = -898;
const ERROR_INTERNAL: i32 = -896;
const ERROR_INVALID_STATE: i32 = -895;
const ERROR_INVALID_HANDLE: i32 = -892;
const ERROR_UNIMPLEMENTED: i32 = -890;
const ERROR_UNAVAILABLE: i32 = -889;
const ERROR_NULL: i32 = -886;
const ERROR_TIMEOUT: i32 = -885;
const ERROR_INVALID_FORMAT: i32 = -883;
const ERROR_OUT_OF_RANGE: i32 = -882;
const ERROR_INVALID_RATE: i32 = -880;

const STATE_OPEN: i32 = 2;
const STATE_STARTED: i32 = 4;
const STATE_PAUSED: i32 = 6;
const STATE_FLUSHED: i32 = 8;
const STATE_STOPPING: i32 = 9;
const STATE_STOPPED: i32 = 10;
const STATE_CLOSED: i32 = 12;
const STATE_DISCONNECTED: i32 = 13;

const OUTPUT: i32 = 0;
const INPUT: i32 = 1;
const FORMAT_UNSPECIFIED: i32 = 0;
const FORMAT_I16: i32 = 1;
const FORMAT_FLOAT: i32 = 2;
const FORMAT_I24: i32 = 3;
const FORMAT_I32: i32 = 4;
const SHARING_SHARED: i32 = 1;
const PERFORMANCE_NONE: i32 = 10;
const CALLBACK_STOP: i32 = 1;
const DEFAULT_RATE: u32 = 48_000;

type DataCallback = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, i32) -> i32;
type ErrorCallback = unsafe extern "C" fn(*mut c_void, *mut c_void, i32);

/// What a builder holds; also the resolved settings of an open stream.
#[derive(Clone, Copy)]
struct Settings {
    device: i32,
    rate: i32,
    channels: i32,
    channel_mask: u32,
    format: i32,
    sharing: i32,
    direction: i32,
    capacity: i32,
    performance: i32,
    usage: i32,
    content: i32,
    preset: i32,
    policy: i32,
    session: i32,
    privacy: bool,
    spatialization: i32,
    spatialized: bool,
    frames_per_callback: i32,
    data: Option<(DataCallback, usize)>,
    error: Option<(ErrorCallback, usize)>,
}

impl Default for Settings {
    fn default() -> Self {
        // AAudio's documented defaults; zero is AAUDIO_UNSPECIFIED.
        Settings {
            device: 0,
            rate: 0,
            channels: 0,
            channel_mask: 0,
            format: FORMAT_UNSPECIFIED,
            sharing: SHARING_SHARED,
            direction: OUTPUT,
            capacity: 0,
            performance: PERFORMANCE_NONE,
            usage: 1,
            content: 2,
            preset: 6,
            policy: 1,
            session: -1,
            privacy: false,
            spatialization: 0,
            spatialized: false,
            frames_per_callback: 0,
            data: None,
            error: None,
        }
    }
}

static BUILDERS: Live<Mutex<Settings>> = Live::new();

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// A stream's AAudio state, shared with its device thread, which never holds the stream itself.
struct Status {
    state: Mutex<i32>,
    changed: Condvar,
    /// Frames the app's data callback produced or consumed.
    frames: AtomicI64,
}

impl Status {
    fn set(&self, state: i32) {
        *lock(&self.state) = state;
        self.changed.notify_all();
    }
}

enum Io {
    Output(Output),
    Input(Input),
}

struct Stream {
    settings: Settings,
    format: Format,
    config: Config,
    io: OnceLock<Io>,
    status: Arc<Status>,
    frames_read: AtomicI64,
    /// Whether a thread watches the device for the app's error callback.
    watched: AtomicBool,
}

static STREAMS: Live<Stream> = Live::new();

/// How often the watch looks at a device that may fail.
const WATCH_PERIOD: Duration = Duration::from_millis(20);

/// Looks at a stream's state until its device fails, which is true, or until the stream is closed
/// or gone (`None`), which is false.
fn await_disconnect(mut state: impl FnMut() -> Option<i32>) -> bool {
    loop {
        match state() {
            Some(STATE_DISCONNECTED) => return true,
            None | Some(STATE_CLOSED) => return false,
            Some(_) => std::thread::sleep(WATCH_PERIOD),
        }
    }
}

/// Calls the app's error callback once, from its own thread as AAudio does, when the device fails,
/// so the app can close the stream and open another, as oboe does.
fn watch(stream: &Arc<Stream>, pointer: *mut c_void) {
    let Some((callback, user)) = stream.settings.error else {
        return;
    };
    if stream.watched.swap(true, Ordering::AcqRel) {
        return;
    }
    let weak: Weak<Stream> = Arc::downgrade(stream);
    let pointer = pointer as usize;
    let spawned = std::thread::Builder::new()
        .name("aaudio-error".into())
        .spawn(move || {
            if await_disconnect(|| weak.upgrade().map(|stream| stream.state())) {
                // SAFETY: the app's callback, with its stream and data, as AAudio calls it.
                unsafe {
                    callback(
                        pointer as *mut c_void,
                        user as *mut c_void,
                        ERROR_DISCONNECTED,
                    )
                };
            }
        });
    if let Err(error) = spawned {
        eprintln!("uxdroid: AAudio: no thread to report a failed device: {error}");
    }
}

fn stream(pointer: *mut c_void) -> Option<Arc<Stream>> {
    STREAMS.get(pointer)
}

impl Stream {
    fn frame_bytes(&self) -> usize {
        usize::from(self.config.channels) * self.format.bytes()
    }

    /// The state, following the device: a drain that finished, or a device that failed.
    fn state(&self) -> i32 {
        let mut state = lock(&self.status.state);
        let device = match self.io.get() {
            Some(Io::Output(output)) => output.state(),
            Some(Io::Input(input)) => input.state(),
            None => State::Stopped,
        };
        if device == State::Disconnected {
            *state = STATE_DISCONNECTED;
        } else if *state == STATE_STOPPING && device == State::Stopped {
            *state = STATE_STOPPED;
        }
        *state
    }
}

/// The app's sample format, or `None` for a compressed or unknown one.
fn sample_format(format: i32) -> Option<Format> {
    Some(match format {
        FORMAT_I16 => Format::I16,
        FORMAT_FLOAT => Format::F32,
        FORMAT_I24 => Format::I24,
        FORMAT_I32 => Format::I32,
        _ => return None,
    })
}

/// Whether the app may record: the manifest must request `RECORD_AUDIO`.
pub(crate) fn may_record() -> bool {
    if cfg!(test) {
        return true;
    }
    let Some(jni) = crate::java::Jni::current() else {
        return false;
    };
    let Ok(class) = jni.find_class(c"dev/uxdroid/system/Permissions") else {
        return false;
    };
    let Ok(name) = jni.string(c"android.permission.RECORD_AUDIO") else {
        jni.delete_local(class);
        return false;
    };
    // SAFETY: a live class and string, and the method's signature.
    let granted = unsafe {
        jni.call_static(
            class,
            c"requested",
            c"(Ljava/lang/String;)Z",
            &[jni_sys::jvalue { l: name }],
        )
    }
    .is_ok_and(|value| unsafe { value.z });
    jni.delete_local(name);
    jni.delete_local(class);
    granted
}

pub(crate) unsafe extern "C" fn create_builder(out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return ERROR_NULL;
    }
    let builder = BUILDERS.add(Arc::new(Mutex::new(Settings::default())));
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = builder };
    OK
}

pub(crate) unsafe extern "C" fn delete_builder(builder: *mut c_void) -> i32 {
    match BUILDERS.remove(builder) {
        Some(_) => OK,
        None => ERROR_INVALID_HANDLE,
    }
}

macro_rules! setters {
    ($($name:ident($type:ty) => $field:ident,)*) => {$(
        pub(crate) unsafe extern "C" fn $name(builder: *mut c_void, value: $type) {
            if let Some(settings) = BUILDERS.get(builder) {
                lock(&settings).$field = value;
            }
        }
    )*};
}

setters! {
    set_device(i32) => device,
    set_rate(i32) => rate,
    set_channels(i32) => channels,
    set_channel_mask(u32) => channel_mask,
    set_format(i32) => format,
    set_sharing(i32) => sharing,
    set_direction(i32) => direction,
    set_capacity(i32) => capacity,
    set_performance(i32) => performance,
    set_usage(i32) => usage,
    set_content(i32) => content,
    set_preset(i32) => preset,
    set_policy(i32) => policy,
    set_session(i32) => session,
    set_privacy(bool) => privacy,
    set_spatialization(i32) => spatialization,
    set_spatialized(bool) => spatialized,
    set_frames_per_callback(i32) => frames_per_callback,
}

/// Package names and attribution tags only matter to Android's permission checks.
pub(crate) unsafe extern "C" fn set_name(_builder: *mut c_void, _name: *const c_char) {}

pub(crate) unsafe extern "C" fn set_data_callback(
    builder: *mut c_void,
    callback: Option<DataCallback>,
    user: *mut c_void,
) {
    if let Some(settings) = BUILDERS.get(builder) {
        lock(&settings).data = callback.map(|callback| (callback, user as usize));
    }
}

pub(crate) unsafe extern "C" fn set_error_callback(
    builder: *mut c_void,
    callback: Option<ErrorCallback>,
    user: *mut c_void,
) {
    if let Some(settings) = BUILDERS.get(builder) {
        lock(&settings).error = callback.map(|callback| (callback, user as usize));
    }
}

/// Fills one period from an app callback that may want another size.
struct OutputAdapter {
    callback: DataCallback,
    user: usize,
    stream: usize,
    format: Format,
    frames: usize,
    channels: usize,
    scratch: Vec<u8>,
    pending: Vec<f32>,
    status: Arc<Status>,
}

impl OutputAdapter {
    fn fill(&mut self, out: &mut [f32]) -> Flow {
        let mut flow = Flow::Continue;
        while self.pending.len() < out.len() {
            if flow == Flow::Stop {
                self.pending.resize(out.len(), 0.0);
                break;
            }
            // SAFETY: the app's callback with its stream, its data and a buffer of `frames` frames.
            let result = unsafe {
                (self.callback)(
                    self.stream as *mut c_void,
                    self.user as *mut c_void,
                    self.scratch.as_mut_ptr().cast(),
                    self.frames as i32,
                )
            };
            self.status
                .frames
                .fetch_add(self.frames as i64, Ordering::Relaxed);
            let start = self.pending.len();
            self.pending
                .resize(start + self.frames * self.channels, 0.0);
            self.format
                .decode(&self.scratch, &mut self.pending[start..]);
            if result == CALLBACK_STOP {
                flow = Flow::Stop;
            }
        }
        out.copy_from_slice(&self.pending[..out.len()]);
        self.pending.drain(..out.len());
        if flow == Flow::Stop {
            self.status.set(STATE_STOPPED);
        }
        flow
    }
}

/// Hands each captured period to an app callback that may want another size.
struct InputAdapter {
    callback: DataCallback,
    user: usize,
    stream: usize,
    format: Format,
    frames: usize,
    channels: usize,
    scratch: Vec<u8>,
    pending: Vec<f32>,
    status: Arc<Status>,
}

impl InputAdapter {
    fn take(&mut self, data: &[f32]) -> Flow {
        self.pending.extend_from_slice(data);
        let chunk = self.frames * self.channels;
        while self.pending.len() >= chunk {
            self.format
                .encode(&self.pending[..chunk], &mut self.scratch);
            self.pending.drain(..chunk);
            // SAFETY: the app's callback with its stream, its data and `frames` captured frames.
            let result = unsafe {
                (self.callback)(
                    self.stream as *mut c_void,
                    self.user as *mut c_void,
                    self.scratch.as_mut_ptr().cast(),
                    self.frames as i32,
                )
            };
            self.status
                .frames
                .fetch_add(self.frames as i64, Ordering::Relaxed);
            if result == CALLBACK_STOP {
                self.status.set(STATE_STOPPED);
                return Flow::Stop;
            }
        }
        Flow::Continue
    }
}

/// Resolves what the app left unspecified, and refuses what Android refuses.
fn resolve(settings: &mut Settings) -> Result<(Format, Config), i32> {
    if settings.direction != OUTPUT && settings.direction != INPUT {
        return Err(ERROR_ILLEGAL_ARGUMENT);
    }
    if settings.format == FORMAT_UNSPECIFIED {
        settings.format = FORMAT_FLOAT;
    }
    let format = sample_format(settings.format).ok_or(ERROR_INVALID_FORMAT)?;
    if settings.rate == 0 {
        settings.rate = DEFAULT_RATE as i32;
    }
    let rate = u32::try_from(settings.rate).map_err(|_| ERROR_INVALID_RATE)?;
    if !(uxdroid_audio::MIN_RATE..=uxdroid_audio::MAX_RATE).contains(&rate) {
        return Err(ERROR_INVALID_RATE);
    }
    if settings.channel_mask != 0 {
        settings.channels = settings.channel_mask.count_ones() as i32;
    } else if settings.channels == 0 {
        settings.channels = if settings.direction == OUTPUT { 2 } else { 1 };
    }
    let channels = u16::try_from(settings.channels)
        .ok()
        .filter(|c| (1..=uxdroid_audio::MAX_CHANNELS).contains(c))
        .ok_or(ERROR_OUT_OF_RANGE)?;
    if settings.frames_per_callback < 0 || settings.capacity < 0 {
        return Err(ERROR_OUT_OF_RANGE);
    }
    let mut config = Config {
        rate,
        channels,
        capacity: 0,
    };
    // Four periods by default: room for two in flight while the app writes two.
    config.capacity = (settings.capacity as u32).max(4 * config.period());
    settings.capacity = config.capacity as i32;
    settings.sharing = SHARING_SHARED;
    Ok((format, config))
}

pub(crate) unsafe extern "C" fn open_stream(builder: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return ERROR_NULL;
    }
    let Some(builder) = BUILDERS.get(builder) else {
        return ERROR_INVALID_HANDLE;
    };
    let mut settings = *lock(&builder);
    let (format, config) = match resolve(&mut settings) {
        Ok(resolved) => resolved,
        Err(error) => return error,
    };
    if settings.direction == INPUT && !may_record() {
        eprintln!(
            "uxdroid: AAudio input refused: the app does not request android.permission.RECORD_AUDIO"
        );
        return ERROR_UNAVAILABLE;
    }
    let status = Arc::new(Status {
        state: Mutex::new(STATE_OPEN),
        changed: Condvar::new(),
        frames: AtomicI64::new(0),
    });
    let stream = Arc::new(Stream {
        settings,
        format,
        config,
        io: OnceLock::new(),
        status: status.clone(),
        frames_read: AtomicI64::new(0),
        watched: AtomicBool::new(false),
    });
    let pointer = STREAMS.add(stream.clone());
    let frames = match settings.frames_per_callback {
        0 => config.period() as usize,
        frames => frames as usize,
    };
    let channels = usize::from(config.channels);
    let backend = Backend::current();
    let opened = match (settings.direction, settings.data) {
        (OUTPUT, Some((callback, user))) => {
            let mut adapter = OutputAdapter {
                callback,
                user,
                stream: pointer as usize,
                format,
                frames,
                channels,
                scratch: vec![0; frames * channels * format.bytes()],
                pending: Vec::new(),
                status,
            };
            Output::with_callback(backend, config, move |out| adapter.fill(out)).map(Io::Output)
        }
        (OUTPUT, None) => Output::open(backend, config).map(Io::Output),
        (_, Some((callback, user))) => {
            let mut adapter = InputAdapter {
                callback,
                user,
                stream: pointer as usize,
                format,
                frames,
                channels,
                scratch: vec![0; frames * channels * format.bytes()],
                pending: Vec::new(),
                status,
            };
            Input::with_callback(backend, config, move |data| adapter.take(data)).map(Io::Input)
        }
        (_, None) => Input::open(backend, config).map(Io::Input),
    };
    match opened {
        Ok(io) => {
            let _ = stream.io.set(io);
            // SAFETY: checked non-null; the API makes it writable.
            unsafe { *out = pointer };
            OK
        }
        Err(error) => {
            eprintln!("uxdroid: AAudio: {error}");
            drop(STREAMS.remove(pointer));
            ERROR_UNAVAILABLE
        }
    }
}

pub(crate) unsafe extern "C" fn close(pointer: *mut c_void) -> i32 {
    match STREAMS.remove(pointer) {
        Some(stream) => {
            stream.status.set(STATE_CLOSED);
            // The device thread stops when the stream drops, here, on the app's thread.
            drop(stream);
            OK
        }
        None => ERROR_INVALID_HANDLE,
    }
}

/// Releases the stream's device and keeps the handle for getters, which `close` then frees.
pub(crate) unsafe extern "C" fn release(pointer: *mut c_void) -> i32 {
    match stream(pointer) {
        Some(stream) => {
            // SAFETY: as for request_stop.
            unsafe { request_stop(pointer) };
            stream.status.set(STATE_CLOSED);
            OK
        }
        None => ERROR_INVALID_HANDLE,
    }
}

pub(crate) unsafe extern "C" fn request_start(pointer: *mut c_void) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    let started = match stream.io.get() {
        Some(Io::Output(output)) => output.start(),
        Some(Io::Input(input)) => input.start(),
        None => false,
    };
    if !started {
        return ERROR_DISCONNECTED;
    }
    stream.status.set(STATE_STARTED);
    watch(&stream, pointer);
    OK
}

pub(crate) unsafe extern "C" fn request_pause(pointer: *mut c_void) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    match stream.io.get() {
        Some(Io::Output(output)) => {
            output.pause();
            stream.status.set(STATE_PAUSED);
            OK
        }
        // AAudio does not pause input streams.
        _ => ERROR_UNIMPLEMENTED,
    }
}

pub(crate) unsafe extern "C" fn request_flush(pointer: *mut c_void) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    let Some(Io::Output(output)) = stream.io.get() else {
        return ERROR_UNIMPLEMENTED;
    };
    if !matches!(
        stream.state(),
        STATE_OPEN | STATE_PAUSED | STATE_STOPPED | STATE_FLUSHED
    ) {
        return ERROR_INVALID_STATE;
    }
    output.flush();
    stream.status.set(STATE_FLUSHED);
    OK
}

/// Stops an input or callback stream at once, and a written output once it played its buffer.
pub(crate) unsafe extern "C" fn request_stop(pointer: *mut c_void) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    match stream.io.get() {
        Some(Io::Output(output)) => {
            output.stop();
            stream.status.set(STATE_STOPPING);
        }
        Some(Io::Input(input)) => {
            input.stop();
            stream.status.set(STATE_STOPPED);
        }
        None => return ERROR_INTERNAL,
    }
    OK
}

pub(crate) unsafe extern "C" fn get_state(pointer: *mut c_void) -> i32 {
    stream(pointer).map_or(STATE_CLOSED, |stream| stream.state())
}

pub(crate) unsafe extern "C" fn wait_for_state_change(
    pointer: *mut c_void,
    from: i32,
    next: *mut i32,
    timeout_ns: i64,
) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    let deadline = Instant::now() + Duration::from_nanos(timeout_ns.max(0) as u64);
    let mut state = stream.state();
    while state == from {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        // The device moves some states on its own, such as a drain that finishes, so look again
        // at least every millisecond.
        let guard = lock(&stream.status.state);
        drop(
            stream
                .status
                .changed
                .wait_timeout(guard, left.min(Duration::from_millis(1)))
                .unwrap_or_else(|p| p.into_inner()),
        );
        state = stream.state();
    }
    if !next.is_null() {
        // SAFETY: checked non-null; the API makes it writable.
        unsafe { *next = state };
    }
    if state == from { ERROR_TIMEOUT } else { OK }
}

fn timeout(nanos: i64) -> Option<Duration> {
    Some(Duration::from_nanos(nanos.max(0) as u64))
}

pub(crate) unsafe extern "C" fn write(
    pointer: *mut c_void,
    buffer: *const c_void,
    frames: i32,
    timeout_ns: i64,
) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    let Some(Io::Output(output)) = stream.io.get() else {
        return ERROR_UNIMPLEMENTED;
    };
    if stream.settings.data.is_some() {
        return ERROR_INVALID_STATE;
    }
    if buffer.is_null() || frames < 0 {
        return ERROR_ILLEGAL_ARGUMENT;
    }
    let bytes = frames as usize * stream.frame_bytes();
    // SAFETY: the API gives `frames` frames of the stream's format.
    let raw = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), bytes) };
    let mut samples = vec![0.0; frames as usize * usize::from(stream.config.channels)];
    stream.format.decode(raw, &mut samples);
    let written = output.write_within(&samples, timeout(timeout_ns));
    if written == 0 && frames > 0 && output.state() == State::Disconnected {
        return ERROR_DISCONNECTED;
    }
    (written / usize::from(stream.config.channels)) as i32
}

pub(crate) unsafe extern "C" fn read(
    pointer: *mut c_void,
    buffer: *mut c_void,
    frames: i32,
    timeout_ns: i64,
) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    let Some(Io::Input(input)) = stream.io.get() else {
        return ERROR_UNIMPLEMENTED;
    };
    if stream.settings.data.is_some() {
        return ERROR_INVALID_STATE;
    }
    if buffer.is_null() || frames < 0 {
        return ERROR_ILLEGAL_ARGUMENT;
    }
    let channels = usize::from(stream.config.channels);
    let mut samples = vec![0.0; frames as usize * channels];
    let read = input.read_within(&mut samples, timeout(timeout_ns));
    if read == 0 && frames > 0 && input.state() == State::Disconnected {
        return ERROR_DISCONNECTED;
    }
    // SAFETY: the API gives room for `frames` frames of the stream's format.
    let raw = unsafe {
        std::slice::from_raw_parts_mut(buffer.cast::<u8>(), frames as usize * stream.frame_bytes())
    };
    stream.format.encode(&samples[..read], raw);
    stream
        .frames_read
        .fetch_add((read / channels) as i64, Ordering::Relaxed);
    (read / channels) as i32
}

/// The capacity is the buffer; it does not shrink, which AAudio allows by reporting the size kept.
pub(crate) unsafe extern "C" fn set_buffer_size(pointer: *mut c_void, _frames: i32) -> i32 {
    stream(pointer).map_or(ERROR_INVALID_HANDLE, |s| s.config.capacity as i32)
}

macro_rules! getters {
    ($($name:ident -> $type:ty, $missing:expr, |$s:ident| $value:expr;)*) => {$(
        pub(crate) unsafe extern "C" fn $name(pointer: *mut c_void) -> $type {
            match stream(pointer) {
                Some($s) => $value,
                None => $missing,
            }
        }
    )*};
}

getters! {
    get_buffer_size -> i32, ERROR_INVALID_HANDLE, |s| s.config.capacity as i32;
    get_capacity -> i32, ERROR_INVALID_HANDLE, |s| s.config.capacity as i32;
    get_burst -> i32, ERROR_INVALID_HANDLE, |s| s.config.period() as i32;
    get_frames_per_callback -> i32, ERROR_INVALID_HANDLE, |s| s.settings.frames_per_callback;
    get_rate -> i32, ERROR_INVALID_HANDLE, |s| s.settings.rate;
    get_channels -> i32, ERROR_INVALID_HANDLE, |s| s.settings.channels;
    get_channel_mask -> u32, 0, |s| s.settings.channel_mask;
    get_device -> i32, 0, |s| s.settings.device;
    get_format -> i32, FORMAT_UNSPECIFIED, |s| s.settings.format;
    get_sharing -> i32, SHARING_SHARED, |s| s.settings.sharing;
    get_performance -> i32, PERFORMANCE_NONE, |s| s.settings.performance;
    get_direction -> i32, OUTPUT, |s| s.settings.direction;
    get_session -> i32, -1, |s| s.settings.session;
    get_usage -> i32, 0, |s| s.settings.usage;
    get_content -> i32, 0, |s| s.settings.content;
    get_preset -> i32, 0, |s| s.settings.preset;
    get_policy -> i32, 0, |s| s.settings.policy;
    get_spatialization -> i32, 0, |s| s.settings.spatialization;
    is_spatialized -> bool, false, |s| s.settings.spatialized;
    is_privacy_sensitive -> bool, false, |s| s.settings.privacy;
    is_mmap_used -> bool, false, |_s| false;
    get_xruns -> i32, ERROR_INVALID_HANDLE, |s| match s.io.get() {
        Some(Io::Output(output)) => output.underruns() as i32,
        Some(Io::Input(input)) => input.overruns() as i32,
        None => 0,
    };
    get_frames_written -> i64, ERROR_INVALID_HANDLE as i64, |s| match s.io.get() {
        Some(Io::Output(_)) if s.settings.data.is_some() => s.status.frames.load(Ordering::Relaxed),
        Some(Io::Output(output)) => output.written() as i64,
        Some(Io::Input(input)) => input.position() as i64,
        None => 0,
    };
    get_frames_read -> i64, ERROR_INVALID_HANDLE as i64, |s| match s.io.get() {
        Some(Io::Output(output)) => output.position() as i64,
        Some(Io::Input(_)) if s.settings.data.is_some() => s.status.frames.load(Ordering::Relaxed),
        Some(Io::Input(_)) => s.frames_read.load(Ordering::Relaxed),
        None => 0,
    };
}

pub(crate) unsafe extern "C" fn get_timestamp(
    pointer: *mut c_void,
    clock: i32,
    position: *mut i64,
    time: *mut i64,
) -> i32 {
    let Some(stream) = stream(pointer) else {
        return ERROR_INVALID_HANDLE;
    };
    if position.is_null() || time.is_null() {
        return ERROR_NULL;
    }
    if stream.state() != STATE_STARTED {
        return ERROR_INVALID_STATE;
    }
    let frame = match stream.io.get() {
        Some(Io::Output(output)) => output.presented(),
        Some(Io::Input(input)) => input.position(),
        None => return ERROR_INTERNAL,
    };
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid output; an invalid clock fails and is reported.
    if unsafe { libc::clock_gettime(clock, &mut now) } != 0 {
        return ERROR_ILLEGAL_ARGUMENT;
    }
    // SAFETY: both checked non-null; the API makes them writable.
    unsafe {
        *position = frame as i64;
        *time = now.tv_sec * 1_000_000_000 + now.tv_nsec;
    }
    OK
}

pub(crate) unsafe extern "C" fn result_text(result: i32) -> *const c_char {
    let text: &CStr = match result {
        OK => c"AAUDIO_OK",
        ERROR_DISCONNECTED => c"AAUDIO_ERROR_DISCONNECTED",
        ERROR_ILLEGAL_ARGUMENT => c"AAUDIO_ERROR_ILLEGAL_ARGUMENT",
        ERROR_INTERNAL => c"AAUDIO_ERROR_INTERNAL",
        ERROR_INVALID_STATE => c"AAUDIO_ERROR_INVALID_STATE",
        ERROR_INVALID_HANDLE => c"AAUDIO_ERROR_INVALID_HANDLE",
        ERROR_UNIMPLEMENTED => c"AAUDIO_ERROR_UNIMPLEMENTED",
        ERROR_UNAVAILABLE => c"AAUDIO_ERROR_UNAVAILABLE",
        ERROR_NULL => c"AAUDIO_ERROR_NULL",
        ERROR_TIMEOUT => c"AAUDIO_ERROR_TIMEOUT",
        ERROR_INVALID_FORMAT => c"AAUDIO_ERROR_INVALID_FORMAT",
        ERROR_OUT_OF_RANGE => c"AAUDIO_ERROR_OUT_OF_RANGE",
        ERROR_INVALID_RATE => c"AAUDIO_ERROR_INVALID_RATE",
        _ => c"Unrecognized AAudio error.",
    };
    text.as_ptr()
}

pub(crate) unsafe extern "C" fn state_text(state: i32) -> *const c_char {
    const NAMES: [&CStr; 14] = [
        c"AAUDIO_STREAM_STATE_UNINITIALIZED",
        c"AAUDIO_STREAM_STATE_UNKNOWN",
        c"AAUDIO_STREAM_STATE_OPEN",
        c"AAUDIO_STREAM_STATE_STARTING",
        c"AAUDIO_STREAM_STATE_STARTED",
        c"AAUDIO_STREAM_STATE_PAUSING",
        c"AAUDIO_STREAM_STATE_PAUSED",
        c"AAUDIO_STREAM_STATE_FLUSHING",
        c"AAUDIO_STREAM_STATE_FLUSHED",
        c"AAUDIO_STREAM_STATE_STOPPING",
        c"AAUDIO_STREAM_STATE_STOPPED",
        c"AAUDIO_STREAM_STATE_CLOSING",
        c"AAUDIO_STREAM_STATE_CLOSED",
        c"AAUDIO_STREAM_STATE_DISCONNECTED",
    ];
    usize::try_from(state)
        .ok()
        .and_then(|index| NAMES.get(index))
        .unwrap_or(&c"Unrecognized AAudio state.")
        .as_ptr()
}

macro_rules! function {
    ($name:literal, $signature:literal, $function:path) => {
        Export::function($name, $signature, $function as *const ())
    };
}

pub static TABLE: Table = Table {
    library: "libaaudio.so",
    exports: &[
        function!(c"AAudio_convertResultToText", "ptr(i32)", result_text),
        function!(c"AAudio_convertStreamStateToText", "ptr(i32)", state_text),
        function!(c"AAudio_createStreamBuilder", "i32(ptr)", create_builder),
        function!(c"AAudioStreamBuilder_delete", "i32(ptr)", delete_builder),
        function!(
            c"AAudioStreamBuilder_openStream",
            "i32(ptr,ptr)",
            open_stream
        ),
        function!(
            c"AAudioStreamBuilder_setAllowedCapturePolicy",
            "void(ptr,i32)",
            set_policy
        ),
        function!(
            c"AAudioStreamBuilder_setAttributionTag",
            "void(ptr,ptr)",
            set_name
        ),
        function!(
            c"AAudioStreamBuilder_setBufferCapacityInFrames",
            "void(ptr,i32)",
            set_capacity
        ),
        function!(
            c"AAudioStreamBuilder_setChannelCount",
            "void(ptr,i32)",
            set_channels
        ),
        function!(
            c"AAudioStreamBuilder_setChannelMask",
            "void(ptr,u32)",
            set_channel_mask
        ),
        function!(
            c"AAudioStreamBuilder_setContentType",
            "void(ptr,i32)",
            set_content
        ),
        function!(
            c"AAudioStreamBuilder_setDataCallback",
            "void(ptr,ptr,ptr)",
            set_data_callback
        ),
        function!(
            c"AAudioStreamBuilder_setDeviceId",
            "void(ptr,i32)",
            set_device
        ),
        function!(
            c"AAudioStreamBuilder_setDirection",
            "void(ptr,i32)",
            set_direction
        ),
        function!(
            c"AAudioStreamBuilder_setErrorCallback",
            "void(ptr,ptr,ptr)",
            set_error_callback
        ),
        function!(
            c"AAudioStreamBuilder_setFormat",
            "void(ptr,i32)",
            set_format
        ),
        function!(
            c"AAudioStreamBuilder_setFramesPerDataCallback",
            "void(ptr,i32)",
            set_frames_per_callback
        ),
        function!(
            c"AAudioStreamBuilder_setInputPreset",
            "void(ptr,i32)",
            set_preset
        ),
        function!(
            c"AAudioStreamBuilder_setIsContentSpatialized",
            "void(ptr,u8)",
            set_spatialized
        ),
        function!(
            c"AAudioStreamBuilder_setPackageName",
            "void(ptr,ptr)",
            set_name
        ),
        function!(
            c"AAudioStreamBuilder_setPerformanceMode",
            "void(ptr,i32)",
            set_performance
        ),
        function!(
            c"AAudioStreamBuilder_setPrivacySensitive",
            "void(ptr,u8)",
            set_privacy
        ),
        function!(
            c"AAudioStreamBuilder_setSampleRate",
            "void(ptr,i32)",
            set_rate
        ),
        function!(
            c"AAudioStreamBuilder_setSamplesPerFrame",
            "void(ptr,i32)",
            set_channels
        ),
        function!(
            c"AAudioStreamBuilder_setSessionId",
            "void(ptr,i32)",
            set_session
        ),
        function!(
            c"AAudioStreamBuilder_setSharingMode",
            "void(ptr,i32)",
            set_sharing
        ),
        function!(
            c"AAudioStreamBuilder_setSpatializationBehavior",
            "void(ptr,i32)",
            set_spatialization
        ),
        function!(c"AAudioStreamBuilder_setUsage", "void(ptr,i32)", set_usage),
        function!(c"AAudioStream_close", "i32(ptr)", close),
        function!(
            c"AAudioStream_getAllowedCapturePolicy",
            "i32(ptr)",
            get_policy
        ),
        function!(
            c"AAudioStream_getBufferCapacityInFrames",
            "i32(ptr)",
            get_capacity
        ),
        function!(
            c"AAudioStream_getBufferSizeInFrames",
            "i32(ptr)",
            get_buffer_size
        ),
        function!(c"AAudioStream_getChannelCount", "i32(ptr)", get_channels),
        function!(c"AAudioStream_getChannelMask", "u32(ptr)", get_channel_mask),
        function!(c"AAudioStream_getContentType", "i32(ptr)", get_content),
        function!(c"AAudioStream_getDeviceId", "i32(ptr)", get_device),
        function!(c"AAudioStream_getDirection", "i32(ptr)", get_direction),
        function!(c"AAudioStream_getFormat", "i32(ptr)", get_format),
        function!(c"AAudioStream_getFramesPerBurst", "i32(ptr)", get_burst),
        function!(
            c"AAudioStream_getFramesPerDataCallback",
            "i32(ptr)",
            get_frames_per_callback
        ),
        function!(c"AAudioStream_getFramesRead", "i64(ptr)", get_frames_read),
        function!(
            c"AAudioStream_getFramesWritten",
            "i64(ptr)",
            get_frames_written
        ),
        function!(
            c"AAudioStream_getHardwareChannelCount",
            "i32(ptr)",
            get_channels
        ),
        function!(c"AAudioStream_getHardwareFormat", "i32(ptr)", get_format),
        function!(c"AAudioStream_getHardwareSampleRate", "i32(ptr)", get_rate),
        function!(c"AAudioStream_getInputPreset", "i32(ptr)", get_preset),
        function!(
            c"AAudioStream_getPerformanceMode",
            "i32(ptr)",
            get_performance
        ),
        function!(c"AAudioStream_getSampleRate", "i32(ptr)", get_rate),
        function!(c"AAudioStream_getSamplesPerFrame", "i32(ptr)", get_channels),
        function!(c"AAudioStream_getSessionId", "i32(ptr)", get_session),
        function!(c"AAudioStream_getSharingMode", "i32(ptr)", get_sharing),
        function!(
            c"AAudioStream_getSpatializationBehavior",
            "i32(ptr)",
            get_spatialization
        ),
        function!(c"AAudioStream_getState", "i32(ptr)", get_state),
        function!(
            c"AAudioStream_getTimestamp",
            "i32(ptr,i32,ptr,ptr)",
            get_timestamp
        ),
        function!(c"AAudioStream_getUsage", "i32(ptr)", get_usage),
        function!(c"AAudioStream_getXRunCount", "i32(ptr)", get_xruns),
        function!(
            c"AAudioStream_isContentSpatialized",
            "u8(ptr)",
            is_spatialized
        ),
        function!(c"AAudioStream_isMMapUsed", "u8(ptr)", is_mmap_used),
        function!(
            c"AAudioStream_isPrivacySensitive",
            "u8(ptr)",
            is_privacy_sensitive
        ),
        function!(c"AAudioStream_read", "i32(ptr,ptr,i32,i64)", read),
        function!(c"AAudioStream_release", "i32(ptr)", release),
        function!(c"AAudioStream_requestFlush", "i32(ptr)", request_flush),
        function!(c"AAudioStream_requestPause", "i32(ptr)", request_pause),
        function!(c"AAudioStream_requestStart", "i32(ptr)", request_start),
        function!(c"AAudioStream_requestStop", "i32(ptr)", request_stop),
        function!(
            c"AAudioStream_setBufferSizeInFrames",
            "i32(ptr,i32)",
            set_buffer_size
        ),
        function!(
            c"AAudioStream_waitForStateChange",
            "i32(ptr,i32,ptr,i64)",
            wait_for_state_change
        ),
        function!(c"AAudioStream_write", "i32(ptr,ptr,i32,i64)", write),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> *mut c_void {
        let mut builder = std::ptr::null_mut();
        // SAFETY: a valid output pointer.
        assert_eq!(unsafe { create_builder(&mut builder) }, OK);
        builder
    }

    fn open(builder: *mut c_void) -> Result<*mut c_void, i32> {
        let mut stream = std::ptr::null_mut();
        // SAFETY: a live builder and a valid output pointer.
        match unsafe { open_stream(builder, &mut stream) } {
            OK => Ok(stream),
            error => Err(error),
        }
    }

    struct Counter {
        calls: Vec<i32>,
        next: i16,
    }

    unsafe extern "C" fn count(
        _stream: *mut c_void,
        user: *mut c_void,
        audio: *mut c_void,
        frames: i32,
    ) -> i32 {
        // SAFETY: the test's counter, and room for `frames` mono 16-bit frames.
        let counter = unsafe { &mut *user.cast::<Counter>() };
        let samples =
            unsafe { std::slice::from_raw_parts_mut(audio.cast::<i16>(), frames as usize) };
        for sample in samples {
            *sample = counter.next;
            counter.next += 1;
        }
        counter.calls.push(frames);
        if counter.calls.len() == 9 {
            CALLBACK_STOP
        } else {
            0
        }
    }

    #[test]
    fn the_watch_reports_a_disconnect_and_ends_when_the_stream_closes_or_goes() {
        let mut states = [STATE_STARTED, STATE_PAUSED, STATE_DISCONNECTED].into_iter();
        assert!(await_disconnect(|| states.next()));
        let mut states = [STATE_STARTED, STATE_CLOSED, STATE_DISCONNECTED].into_iter();
        assert!(!await_disconnect(|| states.next()));
        let mut states = [STATE_STARTED].into_iter();
        assert!(!await_disconnect(|| states.next()));
    }

    #[test]
    fn a_callback_stream_gets_the_size_it_asked_for_and_stops_when_told() {
        let builder = builder();
        let mut counter = Counter {
            calls: Vec::new(),
            next: 0,
        };
        // SAFETY: a live builder; the counter outlives the stream.
        unsafe {
            set_rate(builder, 8_000);
            set_channels(builder, 1);
            set_format(builder, FORMAT_I16);
            set_frames_per_callback(builder, 30);
            set_data_callback(builder, Some(count), (&raw mut counter).cast());
        }
        let stream = open(builder).unwrap();
        // SAFETY: a live stream.
        unsafe {
            assert_eq!(delete_builder(builder), OK);
            assert_eq!(get_state(stream), STATE_OPEN);
            assert_eq!(get_burst(stream), 80);
            assert_eq!(get_frames_per_callback(stream), 30);
            assert_eq!(
                write(stream, [0i16; 4].as_ptr().cast(), 4, 0),
                ERROR_INVALID_STATE
            );
            assert_eq!(request_start(stream), OK);
            let mut state = 0;
            wait_for_state_change(stream, STATE_STARTED, &mut state, 2_000_000_000);
            assert_eq!(state, STATE_STOPPED);
            // Nine callbacks of 30 frames, over four periods of 80; the rest of the last is silence.
            assert_eq!(get_frames_written(stream), 270);
            assert_eq!(get_frames_read(stream), 320);
            assert_eq!(close(stream), OK);
            assert_eq!(get_state(stream), STATE_CLOSED);
            assert_eq!(close(stream), ERROR_INVALID_HANDLE);
        }
        assert_eq!(counter.calls, [30; 9]);
        assert_eq!(counter.next, 270);
    }

    #[test]
    fn a_written_stream_plays_its_buffer_out_when_stopped() {
        let builder = builder();
        // SAFETY: a live builder.
        unsafe {
            set_rate(builder, 8_000);
            set_channels(builder, 2);
            set_format(builder, FORMAT_FLOAT);
        }
        let stream = open(builder).unwrap();
        let frames = [0.25f32; 2 * 100];
        // SAFETY: a live stream and 100 stereo float frames.
        unsafe {
            assert_eq!(get_capacity(stream), 320);
            assert_eq!(write(stream, frames.as_ptr().cast(), 100, 0), 100);
            assert_eq!(get_frames_written(stream), 100);
            let mut position = 0;
            let mut time = 0;
            assert_eq!(
                get_timestamp(stream, libc::CLOCK_MONOTONIC, &mut position, &mut time),
                ERROR_INVALID_STATE
            );
            assert_eq!(request_start(stream), OK);
            assert_eq!(
                get_timestamp(stream, libc::CLOCK_MONOTONIC, &mut position, &mut time),
                OK
            );
            assert!(time > 0);
            assert_eq!(request_stop(stream), OK);
            let mut state = 0;
            wait_for_state_change(stream, STATE_STOPPING, &mut state, 2_000_000_000);
            assert_eq!(state, STATE_STOPPED);
            assert_eq!(get_frames_read(stream), 100);
            assert_eq!(get_xruns(stream), 0);
            assert_eq!(request_flush(stream), OK);
            assert_eq!(get_state(stream), STATE_FLUSHED);
            close(stream);
            delete_builder(builder);
        }
    }

    #[test]
    fn an_input_stream_reads_whole_frames_in_its_format() {
        let builder = builder();
        // SAFETY: a live builder.
        unsafe {
            set_direction(builder, INPUT);
            set_rate(builder, 16_000);
            set_format(builder, FORMAT_I16);
        }
        let stream = open(builder).unwrap();
        let mut samples = [7i16; 200];
        // SAFETY: a live stream and room for 200 mono frames.
        unsafe {
            assert_eq!(get_channels(stream), 1, "input defaults to mono");
            assert_eq!(request_pause(stream), ERROR_UNIMPLEMENTED);
            assert_eq!(request_start(stream), OK);
            assert_eq!(
                read(stream, samples.as_mut_ptr().cast(), 200, 1_000_000_000),
                200
            );
            assert_eq!(get_frames_read(stream), 200);
            close(stream);
            delete_builder(builder);
        }
        assert!(
            samples.iter().all(|&s| s == 0),
            "the null device records silence"
        );
    }

    #[test]
    fn what_android_refuses_is_refused() {
        let builder = builder();
        // SAFETY: a live builder, then forged and null pointers that must be refused.
        unsafe {
            set_format(builder, 5);
            assert_eq!(open(builder), Err(ERROR_INVALID_FORMAT));
            set_format(builder, FORMAT_I16);
            set_rate(builder, 1_000);
            assert_eq!(open(builder), Err(ERROR_INVALID_RATE));
            set_rate(builder, 48_000);
            set_channels(builder, 9);
            assert_eq!(open(builder), Err(ERROR_OUT_OF_RANGE));
            set_channels(builder, 2);
            set_direction(builder, 7);
            assert_eq!(open(builder), Err(ERROR_ILLEGAL_ARGUMENT));
            assert_eq!(open_stream(builder, std::ptr::null_mut()), ERROR_NULL);
            let forged = builder.wrapping_byte_add(8);
            assert_eq!(request_start(forged), ERROR_INVALID_HANDLE);
            assert_eq!(get_state(forged), STATE_CLOSED);
            assert_eq!(delete_builder(builder), OK);
            assert_eq!(delete_builder(builder), ERROR_INVALID_HANDLE);
            let text = |p| CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(text(result_text(ERROR_TIMEOUT)), "AAUDIO_ERROR_TIMEOUT");
            assert_eq!(
                text(state_text(STATE_STOPPED)),
                "AAUDIO_STREAM_STATE_STOPPED"
            );
            assert_eq!(text(state_text(99)), "Unrecognized AAudio state.");
        }
    }

    #[test]
    fn the_table_covers_what_oboe_loads() {
        // The names oboe's AAudioLoader looks up with dlsym, in Android 16's NDK.
        for name in [
            "AAudio_createStreamBuilder",
            "AAudioStreamBuilder_openStream",
            "AAudioStreamBuilder_setChannelCount",
            "AAudioStreamBuilder_setBufferCapacityInFrames",
            "AAudioStreamBuilder_setDirection",
            "AAudioStreamBuilder_setFormat",
            "AAudioStreamBuilder_setFramesPerDataCallback",
            "AAudioStreamBuilder_setSharingMode",
            "AAudioStreamBuilder_setPerformanceMode",
            "AAudioStreamBuilder_setSampleRate",
            "AAudioStreamBuilder_setDataCallback",
            "AAudioStreamBuilder_setErrorCallback",
            "AAudioStreamBuilder_delete",
            "AAudioStream_close",
            "AAudioStream_read",
            "AAudioStream_write",
            "AAudioStream_waitForStateChange",
            "AAudioStream_getTimestamp",
            "AAudioStream_getXRunCount",
            "AAudioStream_getFramesPerBurst",
            "AAudioStream_requestStart",
            "AAudioStream_requestStop",
            "AAudio_convertResultToText",
        ] {
            assert!(TABLE.find(name.as_bytes()).is_some(), "{name}");
        }
        uxdroid_bridge::table::validate(&TABLE).unwrap();
    }
}
