//! `libOpenSLES`: Android's OpenSL ES profile, an engine with an output mix, buffer-queue audio
//! players and audio recorders, over `uxdroid_audio` streams.
//!
//! The interface IDs are objects, so the table binds them as data: each is a pointer to a GUID of
//! this library. App code compares the pointers, never the GUIDs, as the API intends.
//!
//! Every object is a `#[repr(C)]` struct whose fields are its interfaces: `SLObjectItf` points at the
//! first field, and each other interface at its own field, whose value is the interface's method
//! table. A method finds its object from the field's offset and looks it up in [`Live`], so a stale,
//! foreign or forged pointer is refused. Those method tables and the callbacks apps register are
//! host functions handed out by pointer; [`interfaces`] declares their signatures, as the library
//! tables do, for a translator to wrap them.
//!
//! A player plays the buffers its app enqueues: the stream's device thread reads them in order, and
//! calls the app's callback, on that thread, as each one finishes. A recorder fills them the same
//! way. Recording needs the manifest's `RECORD_AUDIO`, as on Android. Players of encoded data, URIs
//! and file descriptors, which need a decoder, are refused with `SL_RESULT_CONTENT_UNSUPPORTED`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::offset_of;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use uxdroid_audio::{Backend, Config, Flow, Format, Input, Output};
use uxdroid_bridge::table::{Export, Table};

use crate::live::Live;

type Result = u32;

const SUCCESS: Result = 0;
const PRECONDITIONS_VIOLATED: Result = 1;
const PARAMETER_INVALID: Result = 2;
const RESOURCE_ERROR: Result = 4;
const BUFFER_INSUFFICIENT: Result = 7;
const CONTENT_UNSUPPORTED: Result = 9;
const PERMISSION_DENIED: Result = 11;
const FEATURE_UNSUPPORTED: Result = 12;
const STATE_UNREALIZED: u32 = 1;
const STATE_REALIZED: u32 = 2;

const PLAYSTATE_STOPPED: u32 = 1;
const PLAYSTATE_PAUSED: u32 = 2;
const PLAYSTATE_PLAYING: u32 = 3;
const RECORDSTATE_STOPPED: u32 = 1;
const RECORDSTATE_PAUSED: u32 = 2;
const RECORDSTATE_RECORDING: u32 = 3;
const LOCATOR_IODEVICE: u32 = 3;
const LOCATOR_OUTPUTMIX: u32 = 4;
const LOCATOR_BUFFERQUEUE: u32 = 6;
const LOCATOR_ANDROIDSIMPLEBUFFERQUEUE: u32 = 0x8000_07BD;
const DATAFORMAT_PCM: u32 = 2;
const ANDROID_DATAFORMAT_PCM_EX: u32 = 4;
const BYTEORDER_LITTLEENDIAN: u32 = 2;
const REPRESENTATION_FLOAT: u32 = 3;
const IODEVICE_AUDIOINPUT: u32 = 1;
const DEFAULT_OUTPUT_DEVICE: u32 = 0xFFFF_FFFD;
const TIME_UNKNOWN: u32 = 0xFFFF_FFFF;

/// `struct SLInterfaceID_`: a GUID.
#[repr(C)]
pub struct Guid {
    time_low: u32,
    time_mid: u16,
    time_hi_and_version: u16,
    clock_seq: u16,
    node: [u8; 6],
}

/// `const SLInterfaceID`, the type of each `SL_IID_*` object.
#[repr(transparent)]
pub struct InterfaceId(*const Guid);

// SAFETY: it points at an immutable static.
unsafe impl Sync for InterfaceId {}

macro_rules! interface_ids {
    ($($name:ident $index:literal,)*) => {
        mod ids {
            use super::{Guid, InterfaceId};
            $(
                #[allow(non_upper_case_globals)]
                pub static $name: InterfaceId = InterfaceId(&Guid {
                    time_low: 0x7578_6400 + $index,
                    time_mid: 0x726f,
                    time_hi_and_version: 0x4964,
                    clock_seq: 0x8000,
                    node: *b"opensl",
                });
            )*
        }

        pub static TABLE: Table = Table {
            library: "libOpenSLES.so",
            exports: &[
                Export::function(
                    c"slCreateEngine",
                    "u32(ptr,u32,ptr,u32,ptr,ptr)",
                    create_engine as *const (),
                ),
                Export::function(
                    c"slQueryNumSupportedEngineInterfaces",
                    "u32(ptr)",
                    query_interface_count as *const (),
                ),
                Export::function(
                    c"slQuerySupportedEngineInterfaces",
                    "u32(u32,ptr)",
                    query_interface as *const (),
                ),
                $(Export::data(
                    match std::ffi::CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes()) {
                        Ok(name) => name,
                        Err(_) => panic!("an interface name holds a nul"),
                    },
                    "ptr",
                    std::ptr::from_ref(&ids::$name).cast(),
                ),)*
            ],
        };
    };
}

interface_ids! {
    SL_IID_3DCOMMIT 1, SL_IID_3DDOPPLER 2, SL_IID_3DGROUPING 3, SL_IID_3DLOCATION 4,
    SL_IID_3DMACROSCOPIC 5, SL_IID_3DSOURCE 6, SL_IID_ANDROIDACOUSTICECHOCANCELLATION 7,
    SL_IID_ANDROIDAUTOMATICGAINCONTROL 8, SL_IID_ANDROIDBUFFERQUEUESOURCE 9,
    SL_IID_ANDROIDCONFIGURATION 10, SL_IID_ANDROIDEFFECT 11, SL_IID_ANDROIDEFFECTCAPABILITIES 12,
    SL_IID_ANDROIDEFFECTSEND 13, SL_IID_ANDROIDNOISESUPPRESSION 14,
    SL_IID_ANDROIDSIMPLEBUFFERQUEUE 15, SL_IID_AUDIODECODERCAPABILITIES 16, SL_IID_AUDIOENCODER 17,
    SL_IID_AUDIOENCODERCAPABILITIES 18, SL_IID_AUDIOIODEVICECAPABILITIES 19, SL_IID_BASSBOOST 20,
    SL_IID_BUFFERQUEUE 21, SL_IID_DEVICEVOLUME 22, SL_IID_DYNAMICINTERFACEMANAGEMENT 23,
    SL_IID_DYNAMICSOURCE 24, SL_IID_EFFECTSEND 25, SL_IID_ENGINE 26, SL_IID_ENGINECAPABILITIES 27,
    SL_IID_ENVIRONMENTALREVERB 28, SL_IID_EQUALIZER 29, SL_IID_LED 30, SL_IID_METADATAEXTRACTION 31,
    SL_IID_METADATATRAVERSAL 32, SL_IID_MIDIMESSAGE 33, SL_IID_MIDIMUTESOLO 34, SL_IID_MIDITEMPO 35,
    SL_IID_MIDITIME 36, SL_IID_MUTESOLO 37, SL_IID_NULL 38, SL_IID_OBJECT 39, SL_IID_OUTPUTMIX 40,
    SL_IID_PITCH 41, SL_IID_PLAYBACKRATE 42, SL_IID_PLAY 43, SL_IID_PREFETCHSTATUS 44,
    SL_IID_PRESETREVERB 45, SL_IID_RATEPITCH 46, SL_IID_RECORD 47, SL_IID_SEEK 48,
    SL_IID_THREADSYNC 49, SL_IID_VIBRA 50, SL_IID_VIRTUALIZER 51, SL_IID_VISUALIZATION 52,
    SL_IID_VOLUME 53,
}

type Id = *const Guid;
type Object = *const *const ObjectMethods;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Reports an API that is answered without doing what it promises, once per process.
fn unimplemented_once(name: &'static str) {
    static SEEN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut seen = lock(&SEEN);
    if !seen.contains(&name) {
        seen.push(name);
        eprintln!("uxdroid: unimplemented {name}");
    }
}

/// `struct SLObjectItf_`, the methods of every OpenSL ES object.
#[repr(C)]
pub(crate) struct ObjectMethods {
    realize: unsafe extern "C" fn(Object, u32) -> Result,
    resume: unsafe extern "C" fn(Object, u32) -> Result,
    get_state: unsafe extern "C" fn(Object, *mut u32) -> Result,
    get_interface: unsafe extern "C" fn(Object, Id, *mut c_void) -> Result,
    register_callback: unsafe extern "C" fn(Object, *mut c_void, *mut c_void) -> Result,
    abort_async_operation: unsafe extern "C" fn(Object),
    destroy: unsafe extern "C" fn(Object),
    set_priority: unsafe extern "C" fn(Object, i32, u32) -> Result,
    get_priority: unsafe extern "C" fn(Object, *mut i32, *mut u32) -> Result,
    set_loss_of_control_interfaces: unsafe extern "C" fn(Object, i16, *mut c_void, u32) -> Result,
}

/// An OpenSL ES object kind: where its objects live, and the interfaces each one has.
trait Kind: Sized + Send + Sync + 'static {
    fn live() -> &'static Live<Self>;
    fn state(&self) -> &AtomicU32;
    /// The interface field for `id`, if objects of this kind have it.
    fn interface(&self, id: Id) -> Option<*const c_void>;
    /// Whether objects of this kind can have `id`, asked when one is created.
    fn supports(id: Id) -> bool;
    /// Acquires what the object needs, such as its device stream, on `Realize`.
    fn on_realize(&self) -> Result {
        SUCCESS
    }
}

fn find<T: Kind>(object: Object) -> Option<Arc<T>> {
    T::live().get(object.cast())
}

/// The object an interface pointer belongs to, from the interface field's offset.
fn owner<T: Kind>(interface: *const c_void, offset: usize) -> Option<Arc<T>> {
    T::live().get(interface.wrapping_byte_sub(offset))
}

unsafe extern "C" fn realize<T: Kind>(object: Object, _asynchronous: u32) -> Result {
    let Some(object) = find::<T>(object) else {
        return PARAMETER_INVALID;
    };
    if object
        .state()
        .compare_exchange(
            STATE_UNREALIZED,
            STATE_REALIZED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return PRECONDITIONS_VIOLATED;
    }
    let result = object.on_realize();
    if result != SUCCESS {
        object.state().store(STATE_UNREALIZED, Ordering::Release);
    }
    result
}

unsafe extern "C" fn resume<T: Kind>(object: Object, _asynchronous: u32) -> Result {
    if find::<T>(object).is_some() {
        PRECONDITIONS_VIOLATED
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn get_state<T: Kind>(object: Object, state: *mut u32) -> Result {
    let Some(object) = find::<T>(object) else {
        return PARAMETER_INVALID;
    };
    if state.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *state = object.state().load(Ordering::Acquire) };
    SUCCESS
}

unsafe extern "C" fn get_interface<T: Kind>(object: Object, id: Id, out: *mut c_void) -> Result {
    let Some(object) = find::<T>(object) else {
        return PARAMETER_INVALID;
    };
    if out.is_null() {
        return PARAMETER_INVALID;
    }
    if object.state().load(Ordering::Acquire) != STATE_REALIZED {
        return PRECONDITIONS_VIOLATED;
    }
    let Some(interface) = object.interface(id) else {
        return FEATURE_UNSUPPORTED;
    };
    // SAFETY: the API makes `out` a writable interface pointer; the object lives until Destroy.
    unsafe { out.cast::<*const c_void>().write(interface) };
    SUCCESS
}

unsafe extern "C" fn register_callback<T: Kind>(
    object: Object,
    _callback: *mut c_void,
    _context: *mut c_void,
) -> Result {
    // Only asynchronous Realize and Resume call it, and both here are synchronous.
    if find::<T>(object).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn abort_async_operation(_object: Object) {}

unsafe extern "C" fn destroy<T: Kind>(object: Object) {
    // A player's or recorder's stream stops when the object drops.
    drop(T::live().remove(object.cast()));
}

unsafe extern "C" fn set_priority(_object: Object, _priority: i32, _preemptable: u32) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn get_priority(
    _object: Object,
    _priority: *mut i32,
    _preemptable: *mut u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn set_loss_of_control_interfaces(
    _object: Object,
    _count: i16,
    _ids: *mut c_void,
    _enabled: u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

const fn object_methods<T: Kind>() -> ObjectMethods {
    ObjectMethods {
        realize: realize::<T>,
        resume: resume::<T>,
        get_state: get_state::<T>,
        get_interface: get_interface::<T>,
        register_callback: register_callback::<T>,
        abort_async_operation,
        destroy: destroy::<T>,
        set_priority,
        get_priority,
        set_loss_of_control_interfaces,
    }
}

/// The interfaces an object is created with must all be ones its kind has, when required.
///
/// # Safety
/// Both arrays must hold `count` entries when `count` is not zero.
unsafe fn check_required<T: Kind>(count: u32, ids: *const Id, required: *const u32) -> Result {
    if count > 0 && (ids.is_null() || required.is_null()) {
        return PARAMETER_INVALID;
    }
    // A list far longer than the API has interfaces is malformed.
    if count > 1024 {
        return PARAMETER_INVALID;
    }
    for index in 0..count as usize {
        // SAFETY: as above.
        let (id, needed) = unsafe { (*ids.add(index), *required.add(index)) };
        if needed != 0 && !T::supports(id) {
            return FEATURE_UNSUPPORTED;
        }
    }
    SUCCESS
}

fn is(id: Id, known: &InterfaceId) -> bool {
    id == known.0
}

// The engine.

/// `struct SLEngineItf_`: fifteen methods. Players, recorders and output mixes are made; the rest,
/// such as LED and vibra devices or MIDI players, answer that the feature is unsupported.
#[repr(C)]
struct EngineMethods {
    create_led_device: unsafe extern "C" fn() -> Result,
    create_vibra_device: unsafe extern "C" fn() -> Result,
    create_audio_player: unsafe extern "C" fn(
        *const c_void,
        *mut Object,
        *const DataSource,
        *const DataSink,
        u32,
        *const Id,
        *const u32,
    ) -> Result,
    create_audio_recorder: unsafe extern "C" fn(
        *const c_void,
        *mut Object,
        *const DataSource,
        *const DataSink,
        u32,
        *const Id,
        *const u32,
    ) -> Result,
    rest_before_mix: [unsafe extern "C" fn() -> Result; 3],
    create_output_mix:
        unsafe extern "C" fn(*const c_void, *mut Object, u32, *const Id, *const u32) -> Result,
    rest: [unsafe extern "C" fn() -> Result; 7],
}

/// An engine object: `SLObjectItf` points at `object`, `SLEngineItf` at `engine`.
#[repr(C)]
struct Engine {
    object: *const ObjectMethods,
    engine: *const EngineMethods,
    state: AtomicU32,
}

// SAFETY: the method tables are immutable statics, and the state is atomic.
unsafe impl Send for Engine {}
// SAFETY: as above.
unsafe impl Sync for Engine {}

static ENGINES: Live<Engine> = Live::new();

impl Kind for Engine {
    fn live() -> &'static Live<Self> {
        &ENGINES
    }
    fn state(&self) -> &AtomicU32 {
        &self.state
    }
    fn interface(&self, id: Id) -> Option<*const c_void> {
        if is(id, &ids::SL_IID_ENGINE) {
            Some(std::ptr::from_ref(&self.engine).cast())
        } else if is(id, &ids::SL_IID_OBJECT) {
            Some(std::ptr::from_ref(&self.object).cast())
        } else {
            None
        }
    }
    fn supports(id: Id) -> bool {
        is(id, &ids::SL_IID_ENGINE) || is(id, &ids::SL_IID_OBJECT)
    }
}

extern "C" fn unsupported() -> Result {
    FEATURE_UNSUPPORTED
}

static ENGINE_OBJECT: ObjectMethods = object_methods::<Engine>();

static ENGINE_METHODS: EngineMethods = EngineMethods {
    create_led_device: unsupported,
    create_vibra_device: unsupported,
    create_audio_player,
    create_audio_recorder,
    rest_before_mix: [unsupported; 3],
    create_output_mix,
    rest: [unsupported; 7],
};

fn engine_of(interface: *const c_void) -> Option<Arc<Engine>> {
    owner::<Engine>(interface, offset_of!(Engine, engine))
        .filter(|engine| engine.state.load(Ordering::Acquire) == STATE_REALIZED)
}

/// `SLresult slCreateEngine(SLObjectItf* pEngine, SLuint32 numOptions, const SLEngineOption*,
/// SLuint32 numInterfaces, const SLInterfaceID*, const SLboolean* pInterfaceRequired)`.
pub(crate) unsafe extern "C" fn create_engine(
    out: *mut Object,
    _option_count: u32,
    _options: *const c_void,
    interface_count: u32,
    interfaces: *const Id,
    required: *const u32,
) -> Result {
    if out.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: the API gives both arrays `interface_count` entries.
    let checked = unsafe { check_required::<Engine>(interface_count, interfaces, required) };
    if checked != SUCCESS {
        return checked;
    }
    let engine = Arc::new(Engine {
        object: &ENGINE_OBJECT,
        engine: &ENGINE_METHODS,
        state: AtomicU32::new(STATE_UNREALIZED),
    });
    let pointer = ENGINES.add(engine);
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = pointer.cast_const().cast() };
    SUCCESS
}

/// `SLresult slQueryNumSupportedEngineInterfaces(SLuint32*)`.
pub(crate) unsafe extern "C" fn query_interface_count(count: *mut u32) -> Result {
    if count.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *count = 2 };
    SUCCESS
}

/// `SLresult slQuerySupportedEngineInterfaces(SLuint32 index, SLInterfaceID*)`.
pub(crate) unsafe extern "C" fn query_interface(index: u32, id: *mut Id) -> Result {
    if id.is_null() {
        return PARAMETER_INVALID;
    }
    let found = match index {
        0 => ids::SL_IID_ENGINE.0,
        1 => ids::SL_IID_OBJECT.0,
        _ => return PARAMETER_INVALID,
    };
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *id = found };
    SUCCESS
}

// The output mix: the host's sound server mixes, so it only names the default device.

#[repr(C)]
struct MixMethods {
    get_destination_output_device_ids:
        unsafe extern "C" fn(*const c_void, *mut i32, *mut u32) -> Result,
    register_device_change_callback:
        unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void) -> Result,
    re_route: unsafe extern "C" fn(*const c_void, i32, *mut u32) -> Result,
}

#[repr(C)]
struct Mix {
    object: *const ObjectMethods,
    mix: *const MixMethods,
    state: AtomicU32,
}

// SAFETY: as for Engine.
unsafe impl Send for Mix {}
// SAFETY: as above.
unsafe impl Sync for Mix {}

static MIXES: Live<Mix> = Live::new();
static MIX_OBJECT: ObjectMethods = object_methods::<Mix>();
static MIX_METHODS: MixMethods = MixMethods {
    get_destination_output_device_ids: mix_devices,
    register_device_change_callback: mix_register,
    re_route: mix_re_route,
};

impl Kind for Mix {
    fn live() -> &'static Live<Self> {
        &MIXES
    }
    fn state(&self) -> &AtomicU32 {
        &self.state
    }
    fn interface(&self, id: Id) -> Option<*const c_void> {
        if is(id, &ids::SL_IID_OUTPUTMIX) {
            Some(std::ptr::from_ref(&self.mix).cast())
        } else if is(id, &ids::SL_IID_OBJECT) {
            Some(std::ptr::from_ref(&self.object).cast())
        } else {
            None
        }
    }
    fn supports(id: Id) -> bool {
        is(id, &ids::SL_IID_OUTPUTMIX) || is(id, &ids::SL_IID_OBJECT)
    }
}

fn mix_of(interface: *const c_void) -> Option<Arc<Mix>> {
    owner::<Mix>(interface, offset_of!(Mix, mix))
}

unsafe extern "C" fn mix_devices(
    interface: *const c_void,
    count: *mut i32,
    devices: *mut u32,
) -> Result {
    if mix_of(interface).is_none() || count.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API gives `*count` entries of `devices`, or asks for the count.
    unsafe {
        if !devices.is_null() && *count >= 1 {
            *devices = DEFAULT_OUTPUT_DEVICE;
        }
        *count = 1;
    }
    SUCCESS
}

unsafe extern "C" fn mix_register(
    interface: *const c_void,
    _callback: *mut c_void,
    _context: *mut c_void,
) -> Result {
    // The default device never changes under the host's sound server.
    if mix_of(interface).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn mix_re_route(
    _interface: *const c_void,
    _count: i32,
    _devices: *mut u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn create_output_mix(
    engine: *const c_void,
    out: *mut Object,
    interface_count: u32,
    interfaces: *const Id,
    required: *const u32,
) -> Result {
    if engine_of(engine).is_none() || out.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: the API gives both arrays `interface_count` entries.
    let checked = unsafe { check_required::<Mix>(interface_count, interfaces, required) };
    if checked != SUCCESS {
        return checked;
    }
    let pointer = MIXES.add(Arc::new(Mix {
        object: &MIX_OBJECT,
        mix: &MIX_METHODS,
        state: AtomicU32::new(STATE_UNREALIZED),
    }));
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = pointer.cast_const().cast() };
    SUCCESS
}

// Data sources and sinks.

/// `SLDataSource` and `SLDataSink`: a locator and a format, both tagged by their first word.
#[repr(C)]
pub(crate) struct DataSource {
    locator: *const u32,
    format: *const u32,
}

type DataSink = DataSource;

/// `SLDataFormat_PCM` and `SLAndroidDataFormat_PCM_EX`, which only adds `representation`.
#[repr(C)]
struct Pcm {
    _format_type: u32,
    channels: u32,
    /// Millihertz.
    rate: u32,
    bits: u32,
    container: u32,
    _channel_mask: u32,
    endianness: u32,
    representation: u32,
}

/// The sample format and stream shape of a PCM format, or why it is refused.
///
/// # Safety
/// `format` must be null or point at a format whose first word tags its type.
unsafe fn pcm(format: *const u32) -> std::result::Result<(Format, u32, u16), Result> {
    if format.is_null() {
        return Err(PARAMETER_INVALID);
    }
    // SAFETY: as above; the tag says how much of the structure exists.
    let pcm = unsafe {
        match *format {
            DATAFORMAT_PCM => std::ptr::read(format.cast::<[u32; 7]>()).into(),
            ANDROID_DATAFORMAT_PCM_EX => std::ptr::read(format.cast::<Pcm>()),
            _ => return Err(CONTENT_UNSUPPORTED),
        }
    };
    if pcm.endianness != BYTEORDER_LITTLEENDIAN || pcm.bits != pcm.container {
        return Err(CONTENT_UNSUPPORTED);
    }
    let sample = match (pcm.bits, pcm.representation) {
        (32, REPRESENTATION_FLOAT) => Format::F32,
        (8, 0 | 2) => Format::U8,
        (16, 0 | 1) => Format::I16,
        (24, 0 | 1) => Format::I24,
        (32, 0 | 1) => Format::I32,
        _ => return Err(CONTENT_UNSUPPORTED),
    };
    let channels = u16::try_from(pcm.channels).map_err(|_| PARAMETER_INVALID)?;
    Ok((sample, pcm.rate / 1000, channels))
}

impl From<[u32; 7]> for Pcm {
    fn from(words: [u32; 7]) -> Pcm {
        let [
            format_type,
            channels,
            rate,
            bits,
            container,
            channel_mask,
            endianness,
        ] = words;
        Pcm {
            _format_type: format_type,
            channels,
            rate,
            bits,
            container,
            _channel_mask: channel_mask,
            endianness,
            representation: 0,
        }
    }
}

/// The buffer count of a buffer-queue locator.
///
/// # Safety
/// `locator` must be null or point at a locator whose first word tags its type.
unsafe fn queue_length(locator: *const u32) -> std::result::Result<u32, Result> {
    if locator.is_null() {
        return Err(PARAMETER_INVALID);
    }
    // SAFETY: as above; both buffer-queue locators are two words.
    unsafe {
        match *locator {
            LOCATOR_ANDROIDSIMPLEBUFFERQUEUE | LOCATOR_BUFFERQUEUE if *locator.add(1) > 0 => {
                Ok(*locator.add(1))
            }
            LOCATOR_ANDROIDSIMPLEBUFFERQUEUE | LOCATOR_BUFFERQUEUE => Err(PARAMETER_INVALID),
            _ => Err(CONTENT_UNSUPPORTED),
        }
    }
}

// Buffer queues, shared by players and recorders.

/// `struct SLAndroidSimpleBufferQueueItf_`, also served for `SL_IID_BUFFERQUEUE`: both have these
/// four methods, and states of the same layout.
#[repr(C)]
struct QueueMethods {
    enqueue: unsafe extern "C" fn(*const c_void, *const c_void, u32) -> Result,
    clear: unsafe extern "C" fn(*const c_void) -> Result,
    get_state: unsafe extern "C" fn(*const c_void, *mut [u32; 2]) -> Result,
    register_callback:
        unsafe extern "C" fn(*const c_void, Option<QueueCallback>, *mut c_void) -> Result,
}

/// `slAndroidSimpleBufferQueueCallback`: `void (*)(SLAndroidSimpleBufferQueueItf, void*)`.
type QueueCallback = unsafe extern "C" fn(*const c_void, *mut c_void);

static QUEUE_METHODS: QueueMethods = QueueMethods {
    enqueue,
    clear,
    get_state: queue_state,
    register_callback: queue_register,
};

/// An app buffer: its address stays the app's until the queue hands it back through the callback.
struct Buffer {
    address: usize,
    length: usize,
    done: usize,
}

struct Queue {
    buffers: VecDeque<Buffer>,
    /// Buffers played or filled since the queue was made.
    completed: u32,
    callback: Option<(QueueCallback, usize)>,
}

/// What the app's thread and the stream's device thread share: never the object itself.
struct Fifo {
    queue: Mutex<Queue>,
    length: u32,
    /// The queue interface pointer, which callbacks receive.
    interface: AtomicUsize,
    format: Format,
    channels: usize,
}

impl Fifo {
    fn new(length: u32, format: Format, channels: u16) -> Arc<Fifo> {
        Arc::new(Fifo {
            queue: Mutex::new(Queue {
                buffers: VecDeque::new(),
                completed: 0,
                callback: None,
            }),
            length,
            interface: AtomicUsize::new(0),
            format,
            channels: usize::from(channels),
        })
    }

    fn frame_bytes(&self) -> usize {
        self.channels * self.format.bytes()
    }

    /// Calls the app's callback for one finished buffer, without holding the queue.
    fn finished(&self, callback: Option<(QueueCallback, usize)>) {
        if let Some((callback, context)) = callback {
            // SAFETY: the app's callback, with its queue interface and context.
            unsafe {
                callback(
                    self.interface.load(Ordering::Relaxed) as *const c_void,
                    context as *mut c_void,
                )
            };
        }
    }

    /// Plays the queued buffers into `out`, in order, then silence.
    fn play(&self, out: &mut [f32]) {
        let mut filled = 0;
        while filled < out.len() {
            let mut queue = lock(&self.queue);
            let Some(buffer) = queue.buffers.front_mut() else {
                break;
            };
            let frames = ((buffer.length - buffer.done) / self.frame_bytes())
                .min((out.len() - filled) / self.channels);
            let bytes = frames * self.frame_bytes();
            // SAFETY: the app lent `length` bytes at `address` until the buffer completes.
            let data = unsafe {
                std::slice::from_raw_parts((buffer.address + buffer.done) as *const u8, bytes)
            };
            self.format
                .decode(data, &mut out[filled..filled + frames * self.channels]);
            buffer.done += bytes;
            filled += frames * self.channels;
            if buffer.length - buffer.done < self.frame_bytes() {
                queue.buffers.pop_front();
                queue.completed += 1;
                let callback = queue.callback;
                drop(queue);
                self.finished(callback);
            }
        }
        out[filled..].fill(0.0);
    }

    /// Fills the queued buffers from `data`, in order; what finds no buffer is lost.
    fn record(&self, data: &[f32]) {
        let mut taken = 0;
        while taken < data.len() {
            let mut queue = lock(&self.queue);
            let Some(buffer) = queue.buffers.front_mut() else {
                return;
            };
            let frames = ((buffer.length - buffer.done) / self.frame_bytes())
                .min((data.len() - taken) / self.channels);
            let bytes = frames * self.frame_bytes();
            // SAFETY: the app lent `length` writable bytes at `address` until the buffer completes.
            let room = unsafe {
                std::slice::from_raw_parts_mut((buffer.address + buffer.done) as *mut u8, bytes)
            };
            self.format
                .encode(&data[taken..taken + frames * self.channels], room);
            buffer.done += bytes;
            taken += frames * self.channels;
            if buffer.length - buffer.done < self.frame_bytes() {
                queue.buffers.pop_front();
                queue.completed += 1;
                let callback = queue.callback;
                drop(queue);
                self.finished(callback);
            }
        }
    }
}

/// The queue behind an interface pointer: a player's or a recorder's, which share its offset.
fn fifo_of(interface: *const c_void) -> Option<Arc<Fifo>> {
    const _: () = assert!(offset_of!(Player, queue) == offset_of!(Recorder, queue));
    owner::<Player>(interface, offset_of!(Player, queue))
        .map(|player| player.fifo.clone())
        .or_else(|| {
            owner::<Recorder>(interface, offset_of!(Recorder, queue))
                .map(|recorder| recorder.fifo.clone())
        })
}

unsafe extern "C" fn enqueue(interface: *const c_void, buffer: *const c_void, size: u32) -> Result {
    let Some(fifo) = fifo_of(interface) else {
        return PARAMETER_INVALID;
    };
    if buffer.is_null() || size == 0 {
        return PARAMETER_INVALID;
    }
    let mut queue = lock(&fifo.queue);
    if queue.buffers.len() >= fifo.length as usize {
        return BUFFER_INSUFFICIENT;
    }
    queue.buffers.push_back(Buffer {
        address: buffer as usize,
        length: size as usize,
        done: 0,
    });
    SUCCESS
}

/// Empties the queue and counts from its first buffer again, as Android's `Clear` does.
unsafe extern "C" fn clear(interface: *const c_void) -> Result {
    let Some(fifo) = fifo_of(interface) else {
        return PARAMETER_INVALID;
    };
    let mut queue = lock(&fifo.queue);
    queue.buffers.clear();
    queue.completed = 0;
    SUCCESS
}

/// Whether the queue's player or recorder is stopped; `None` for an unknown interface.
fn queue_stopped(interface: *const c_void) -> Option<bool> {
    owner::<Player>(interface, offset_of!(Player, queue))
        .map(|player| player.play_state.load(Ordering::Acquire) == PLAYSTATE_STOPPED)
        .or_else(|| {
            owner::<Recorder>(interface, offset_of!(Recorder, queue)).map(|recorder| {
                recorder.record_state.load(Ordering::Acquire) == RECORDSTATE_STOPPED
            })
        })
}

unsafe extern "C" fn queue_state(interface: *const c_void, state: *mut [u32; 2]) -> Result {
    let Some(fifo) = fifo_of(interface) else {
        return PARAMETER_INVALID;
    };
    if state.is_null() {
        return PARAMETER_INVALID;
    }
    let queue = lock(&fifo.queue);
    // SAFETY: checked non-null; `count` and `index` (or `playIndex`).
    unsafe { *state = [queue.buffers.len() as u32, queue.completed] };
    SUCCESS
}

unsafe extern "C" fn queue_register(
    interface: *const c_void,
    callback: Option<QueueCallback>,
    context: *mut c_void,
) -> Result {
    let Some(fifo) = fifo_of(interface) else {
        return PARAMETER_INVALID;
    };
    // As Android: the device thread calls the callback, so it changes only while stopped.
    if queue_stopped(interface) != Some(true) {
        return PRECONDITIONS_VIOLATED;
    }
    lock(&fifo.queue).callback = callback.map(|callback| (callback, context as usize));
    SUCCESS
}

// `SLAndroidConfigurationItf`: keys such as the stream type, recording preset and performance mode
// are accepted; the host's server decides routing and latency.

#[repr(C)]
struct ConfigurationMethods {
    set: unsafe extern "C" fn(*const c_void, *const i8, *const c_void, u32) -> Result,
    get: unsafe extern "C" fn(*const c_void, *const i8, *mut u32, *mut c_void) -> Result,
    acquire_java_proxy: unsafe extern "C" fn(*const c_void, u32, *mut c_void) -> Result,
    release_java_proxy: unsafe extern "C" fn(*const c_void, u32) -> Result,
}

static CONFIGURATION_METHODS: ConfigurationMethods = ConfigurationMethods {
    set: configuration_set,
    get: configuration_get,
    acquire_java_proxy: java_proxy,
    release_java_proxy,
};

unsafe extern "C" fn configuration_set(
    _interface: *const c_void,
    key: *const i8,
    value: *const c_void,
    _size: u32,
) -> Result {
    if key.is_null() || value.is_null() {
        PARAMETER_INVALID
    } else {
        SUCCESS
    }
}

unsafe extern "C" fn configuration_get(
    _interface: *const c_void,
    _key: *const i8,
    _size: *mut u32,
    _value: *mut c_void,
) -> Result {
    PARAMETER_INVALID
}

unsafe extern "C" fn java_proxy(
    _interface: *const c_void,
    _kind: u32,
    _out: *mut c_void,
) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn release_java_proxy(_interface: *const c_void, _kind: u32) -> Result {
    FEATURE_UNSUPPORTED
}

// Audio players.

#[repr(C)]
struct PlayMethods {
    set_play_state: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_play_state: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    get_duration: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    get_position: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    register_callback: unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void) -> Result,
    set_callback_events_mask: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_callback_events_mask: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_marker_position: unsafe extern "C" fn(*const c_void, u32) -> Result,
    clear_marker_position: unsafe extern "C" fn(*const c_void) -> Result,
    get_marker_position: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_position_update_period: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_position_update_period: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
}

#[repr(C)]
struct VolumeMethods {
    set_volume_level: unsafe extern "C" fn(*const c_void, i16) -> Result,
    get_volume_level: unsafe extern "C" fn(*const c_void, *mut i16) -> Result,
    get_max_volume_level: unsafe extern "C" fn(*const c_void, *mut i16) -> Result,
    set_mute: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_mute: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    enable_stereo_position: unsafe extern "C" fn(*const c_void, u32) -> Result,
    is_enabled_stereo_position: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_stereo_position: unsafe extern "C" fn(*const c_void, i16) -> Result,
    get_stereo_position: unsafe extern "C" fn(*const c_void, *mut i16) -> Result,
}

#[derive(Clone, Copy, Default)]
struct Volume {
    /// Millibels, 0 at most.
    level: i16,
    mute: bool,
    stereo: bool,
    /// Permille, from -1000 (left) to 1000 (right).
    position: i16,
}

impl Volume {
    fn gains(self) -> (f32, f32) {
        if self.mute {
            return (0.0, 0.0);
        }
        let gain = 10f32.powf(f32::from(self.level) / 2000.0);
        if !self.stereo {
            return (gain, gain);
        }
        let position = f32::from(self.position) / 1000.0;
        (
            gain * (1.0 - position.max(0.0)),
            gain * (1.0 + position.min(0.0)),
        )
    }
}

/// A buffer-queue audio player. Its stream opens on `Realize`.
#[repr(C)]
struct Player {
    object: *const ObjectMethods,
    play: *const PlayMethods,
    queue: *const QueueMethods,
    volume: *const VolumeMethods,
    configuration: *const ConfigurationMethods,
    state: AtomicU32,
    fifo: Arc<Fifo>,
    config: Config,
    output: Mutex<Option<Output>>,
    play_state: AtomicU32,
    /// The stream's position when the player last stopped.
    base: AtomicU64,
    volume_state: Mutex<Volume>,
}

// SAFETY: the method tables are immutable statics; the rest is atomic or locked.
unsafe impl Send for Player {}
// SAFETY: as above.
unsafe impl Sync for Player {}

static PLAYERS: Live<Player> = Live::new();
static PLAYER_OBJECT: ObjectMethods = object_methods::<Player>();

impl Kind for Player {
    fn live() -> &'static Live<Self> {
        &PLAYERS
    }
    fn state(&self) -> &AtomicU32 {
        &self.state
    }
    fn interface(&self, id: Id) -> Option<*const c_void> {
        let field: *const c_void = if is(id, &ids::SL_IID_OBJECT) {
            std::ptr::from_ref(&self.object).cast()
        } else if is(id, &ids::SL_IID_PLAY) {
            std::ptr::from_ref(&self.play).cast()
        } else if is(id, &ids::SL_IID_BUFFERQUEUE) || is(id, &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE)
        {
            std::ptr::from_ref(&self.queue).cast()
        } else if is(id, &ids::SL_IID_VOLUME) {
            std::ptr::from_ref(&self.volume).cast()
        } else if is(id, &ids::SL_IID_ANDROIDCONFIGURATION) {
            std::ptr::from_ref(&self.configuration).cast()
        } else {
            return None;
        };
        Some(field)
    }
    fn supports(id: Id) -> bool {
        [
            &ids::SL_IID_OBJECT,
            &ids::SL_IID_PLAY,
            &ids::SL_IID_BUFFERQUEUE,
            &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE,
            &ids::SL_IID_VOLUME,
            &ids::SL_IID_ANDROIDCONFIGURATION,
        ]
        .iter()
        .any(|known| is(id, known))
    }
    fn on_realize(&self) -> Result {
        let fifo = self.fifo.clone();
        match Output::with_callback(Backend::current(), self.config, move |out| {
            fifo.play(out);
            Flow::Continue
        }) {
            Ok(output) => {
                *lock(&self.output) = Some(output);
                SUCCESS
            }
            Err(error) => {
                eprintln!("uxdroid: OpenSL ES audio player: {error}");
                RESOURCE_ERROR
            }
        }
    }
}

static PLAY_METHODS: PlayMethods = PlayMethods {
    set_play_state,
    get_play_state,
    get_duration,
    get_position: play_position,
    register_callback: play_register,
    set_callback_events_mask: play_events,
    get_callback_events_mask: play_get_u32,
    set_marker_position: play_events,
    clear_marker_position: play_clear_marker,
    get_marker_position: play_get_u32,
    set_position_update_period: play_events,
    get_position_update_period: play_get_u32,
};

static VOLUME_METHODS: VolumeMethods = VolumeMethods {
    set_volume_level,
    get_volume_level,
    get_max_volume_level,
    set_mute,
    get_mute,
    enable_stereo_position,
    is_enabled_stereo_position,
    set_stereo_position,
    get_stereo_position,
};

fn player_of(interface: *const c_void, offset: usize) -> Option<Arc<Player>> {
    owner::<Player>(interface, offset)
}

fn played(interface: *const c_void) -> Option<Arc<Player>> {
    player_of(interface, offset_of!(Player, play))
}

/// Milliseconds of `frames` at `rate`, as OpenSL ES reports positions.
fn millis(frames: u64, rate: u32) -> u32 {
    (frames * 1000 / u64::from(rate)) as u32
}

unsafe extern "C" fn set_play_state(interface: *const c_void, state: u32) -> Result {
    let Some(player) = played(interface) else {
        return PARAMETER_INVALID;
    };
    let output = lock(&player.output);
    let Some(output) = output.as_ref() else {
        return PRECONDITIONS_VIOLATED;
    };
    match state {
        PLAYSTATE_PLAYING => {
            output.start();
        }
        PLAYSTATE_PAUSED => output.pause(),
        PLAYSTATE_STOPPED => {
            output.stop();
            player.base.store(output.position(), Ordering::Relaxed);
        }
        _ => return PARAMETER_INVALID,
    }
    player.play_state.store(state, Ordering::Release);
    SUCCESS
}

unsafe extern "C" fn get_play_state(interface: *const c_void, state: *mut u32) -> Result {
    let Some(player) = played(interface) else {
        return PARAMETER_INVALID;
    };
    if state.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *state = player.play_state.load(Ordering::Acquire) };
    SUCCESS
}

unsafe extern "C" fn get_duration(interface: *const c_void, duration: *mut u32) -> Result {
    if played(interface).is_none() || duration.is_null() {
        return PARAMETER_INVALID;
    }
    // A buffer queue has no end known in advance.
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *duration = TIME_UNKNOWN };
    SUCCESS
}

unsafe extern "C" fn play_position(interface: *const c_void, position: *mut u32) -> Result {
    let Some(player) = played(interface) else {
        return PARAMETER_INVALID;
    };
    if position.is_null() {
        return PARAMETER_INVALID;
    }
    let frames = lock(&player.output).as_ref().map_or(0, |output| {
        output
            .presented()
            .saturating_sub(player.base.load(Ordering::Relaxed))
    });
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *position = millis(frames, player.config.rate) };
    SUCCESS
}

/// Play events, markers and position updates are accepted and never sent.
unsafe extern "C" fn play_register(
    interface: *const c_void,
    callback: *mut c_void,
    _context: *mut c_void,
) -> Result {
    if played(interface).is_none() {
        return PARAMETER_INVALID;
    }
    if !callback.is_null() {
        unimplemented_once("OpenSL ES play events (SLPlayItf callbacks)");
    }
    SUCCESS
}

unsafe extern "C" fn play_events(interface: *const c_void, _value: u32) -> Result {
    if played(interface).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn play_clear_marker(interface: *const c_void) -> Result {
    if played(interface).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn play_get_u32(interface: *const c_void, value: *mut u32) -> Result {
    if played(interface).is_none() || value.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *value = 0 };
    SUCCESS
}

/// Changes the player's volume and applies it to its stream.
fn change_volume(interface: *const c_void, change: impl FnOnce(&mut Volume) -> Result) -> Result {
    let Some(player) = player_of(interface, offset_of!(Player, volume)) else {
        return PARAMETER_INVALID;
    };
    let mut volume = lock(&player.volume_state);
    let result = change(&mut volume);
    if result == SUCCESS {
        let (left, right) = volume.gains();
        if let Some(output) = lock(&player.output).as_ref() {
            output.set_volume(left, right);
        }
    }
    result
}

/// Reads the player's volume into `out`.
fn read_volume<T>(
    interface: *const c_void,
    out: *mut T,
    read: impl FnOnce(&Volume) -> T,
) -> Result {
    let Some(player) = player_of(interface, offset_of!(Player, volume)) else {
        return PARAMETER_INVALID;
    };
    if out.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = read(&lock(&player.volume_state)) };
    SUCCESS
}

unsafe extern "C" fn set_volume_level(interface: *const c_void, level: i16) -> Result {
    change_volume(interface, |volume| {
        if level > 0 {
            return PARAMETER_INVALID;
        }
        volume.level = level;
        SUCCESS
    })
}

unsafe extern "C" fn get_volume_level(interface: *const c_void, level: *mut i16) -> Result {
    read_volume(interface, level, |volume| volume.level)
}

unsafe extern "C" fn get_max_volume_level(interface: *const c_void, level: *mut i16) -> Result {
    read_volume(interface, level, |_| 0)
}

unsafe extern "C" fn set_mute(interface: *const c_void, mute: u32) -> Result {
    change_volume(interface, |volume| {
        volume.mute = mute != 0;
        SUCCESS
    })
}

unsafe extern "C" fn get_mute(interface: *const c_void, mute: *mut u32) -> Result {
    read_volume(interface, mute, |volume| u32::from(volume.mute))
}

unsafe extern "C" fn enable_stereo_position(interface: *const c_void, enable: u32) -> Result {
    change_volume(interface, |volume| {
        volume.stereo = enable != 0;
        SUCCESS
    })
}

unsafe extern "C" fn is_enabled_stereo_position(
    interface: *const c_void,
    enabled: *mut u32,
) -> Result {
    read_volume(interface, enabled, |volume| u32::from(volume.stereo))
}

unsafe extern "C" fn set_stereo_position(interface: *const c_void, position: i16) -> Result {
    change_volume(interface, |volume| {
        if !(-1000..=1000).contains(&position) {
            return PARAMETER_INVALID;
        }
        volume.position = position;
        SUCCESS
    })
}

unsafe extern "C" fn get_stereo_position(interface: *const c_void, position: *mut i16) -> Result {
    read_volume(interface, position, |volume| volume.position)
}

/// `CreateAudioPlayer`: PCM from a buffer queue into the output mix.
unsafe extern "C" fn create_audio_player(
    engine: *const c_void,
    out: *mut Object,
    source: *const DataSource,
    sink: *const DataSink,
    interface_count: u32,
    interfaces: *const Id,
    required: *const u32,
) -> Result {
    if engine_of(engine).is_none() || out.is_null() || source.is_null() || sink.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: the API gives a source and sink of tagged locators and formats, and both arrays with
    // `interface_count` entries.
    let (length, (format, rate, channels)) = unsafe {
        let checked = check_required::<Player>(interface_count, interfaces, required);
        if checked != SUCCESS {
            return checked;
        }
        let length = match queue_length((*source).locator) {
            Ok(length) => length,
            Err(CONTENT_UNSUPPORTED) => {
                unimplemented_once(
                    "OpenSL ES players of URIs and file descriptors, which need a decoder",
                );
                return CONTENT_UNSUPPORTED;
            }
            Err(error) => return error,
        };
        let mix = (*sink).locator;
        if mix.is_null()
            || *mix != LOCATOR_OUTPUTMIX
            || MIXES.get(*mix.add(2).cast::<*const c_void>()).is_none()
        {
            return PARAMETER_INVALID;
        }
        match pcm((*source).format) {
            Ok(shape) => (length, shape),
            Err(error) => return error,
        }
    };
    let mut config = Config {
        rate,
        channels,
        capacity: 0,
    };
    if config.validate().is_err() {
        return PARAMETER_INVALID;
    }
    config.capacity = config.period();
    let fifo = Fifo::new(length, format, channels);
    let player = Arc::new(Player {
        object: &PLAYER_OBJECT,
        play: &PLAY_METHODS,
        queue: &QUEUE_METHODS,
        volume: &VOLUME_METHODS,
        configuration: &CONFIGURATION_METHODS,
        state: AtomicU32::new(STATE_UNREALIZED),
        fifo,
        config,
        output: Mutex::new(None),
        play_state: AtomicU32::new(PLAYSTATE_STOPPED),
        base: AtomicU64::new(0),
        volume_state: Mutex::new(Volume::default()),
    });
    player.fifo.interface.store(
        std::ptr::from_ref(&player.queue) as usize,
        Ordering::Relaxed,
    );
    let pointer = PLAYERS.add(player);
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = pointer.cast_const().cast() };
    SUCCESS
}

// Audio recorders.

#[repr(C)]
struct RecordMethods {
    set_record_state: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_record_state: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_duration_limit: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_position: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    register_callback: unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void) -> Result,
    set_callback_events_mask: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_callback_events_mask: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_marker_position: unsafe extern "C" fn(*const c_void, u32) -> Result,
    clear_marker_position: unsafe extern "C" fn(*const c_void) -> Result,
    get_marker_position: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
    set_position_update_period: unsafe extern "C" fn(*const c_void, u32) -> Result,
    get_position_update_period: unsafe extern "C" fn(*const c_void, *mut u32) -> Result,
}

/// A buffer-queue audio recorder. Its stream opens on `Realize`.
#[repr(C)]
struct Recorder {
    object: *const ObjectMethods,
    record: *const RecordMethods,
    queue: *const QueueMethods,
    configuration: *const ConfigurationMethods,
    state: AtomicU32,
    fifo: Arc<Fifo>,
    config: Config,
    input: Mutex<Option<Input>>,
    record_state: AtomicU32,
    base: AtomicU64,
}

// SAFETY: as for Player.
unsafe impl Send for Recorder {}
// SAFETY: as above.
unsafe impl Sync for Recorder {}

static RECORDERS: Live<Recorder> = Live::new();
static RECORDER_OBJECT: ObjectMethods = object_methods::<Recorder>();

impl Kind for Recorder {
    fn live() -> &'static Live<Self> {
        &RECORDERS
    }
    fn state(&self) -> &AtomicU32 {
        &self.state
    }
    fn interface(&self, id: Id) -> Option<*const c_void> {
        let field: *const c_void = if is(id, &ids::SL_IID_OBJECT) {
            std::ptr::from_ref(&self.object).cast()
        } else if is(id, &ids::SL_IID_RECORD) {
            std::ptr::from_ref(&self.record).cast()
        } else if is(id, &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE) || is(id, &ids::SL_IID_BUFFERQUEUE)
        {
            std::ptr::from_ref(&self.queue).cast()
        } else if is(id, &ids::SL_IID_ANDROIDCONFIGURATION) {
            std::ptr::from_ref(&self.configuration).cast()
        } else {
            return None;
        };
        Some(field)
    }
    fn supports(id: Id) -> bool {
        [
            &ids::SL_IID_OBJECT,
            &ids::SL_IID_RECORD,
            &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE,
            &ids::SL_IID_BUFFERQUEUE,
            &ids::SL_IID_ANDROIDCONFIGURATION,
        ]
        .iter()
        .any(|known| is(id, known))
    }
    fn on_realize(&self) -> Result {
        let fifo = self.fifo.clone();
        match Input::with_callback(Backend::current(), self.config, move |data| {
            fifo.record(data);
            Flow::Continue
        }) {
            Ok(input) => {
                *lock(&self.input) = Some(input);
                SUCCESS
            }
            Err(error) => {
                eprintln!("uxdroid: OpenSL ES audio recorder: {error}");
                RESOURCE_ERROR
            }
        }
    }
}

static RECORD_METHODS: RecordMethods = RecordMethods {
    set_record_state,
    get_record_state,
    set_duration_limit: record_events,
    get_position: record_position,
    register_callback: record_register,
    set_callback_events_mask: record_events,
    get_callback_events_mask: record_get_u32,
    set_marker_position: record_events,
    clear_marker_position: record_clear_marker,
    get_marker_position: record_get_u32,
    set_position_update_period: record_events,
    get_position_update_period: record_get_u32,
};

fn recorded(interface: *const c_void) -> Option<Arc<Recorder>> {
    owner::<Recorder>(interface, offset_of!(Recorder, record))
}

unsafe extern "C" fn set_record_state(interface: *const c_void, state: u32) -> Result {
    let Some(recorder) = recorded(interface) else {
        return PARAMETER_INVALID;
    };
    let input = lock(&recorder.input);
    let Some(input) = input.as_ref() else {
        return PRECONDITIONS_VIOLATED;
    };
    match state {
        RECORDSTATE_RECORDING => {
            input.start();
        }
        RECORDSTATE_PAUSED => input.stop(),
        RECORDSTATE_STOPPED => {
            input.stop();
            recorder.base.store(input.position(), Ordering::Relaxed);
        }
        _ => return PARAMETER_INVALID,
    }
    recorder.record_state.store(state, Ordering::Release);
    SUCCESS
}

unsafe extern "C" fn get_record_state(interface: *const c_void, state: *mut u32) -> Result {
    let Some(recorder) = recorded(interface) else {
        return PARAMETER_INVALID;
    };
    if state.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *state = recorder.record_state.load(Ordering::Acquire) };
    SUCCESS
}

unsafe extern "C" fn record_position(interface: *const c_void, position: *mut u32) -> Result {
    let Some(recorder) = recorded(interface) else {
        return PARAMETER_INVALID;
    };
    if position.is_null() {
        return PARAMETER_INVALID;
    }
    let frames = lock(&recorder.input).as_ref().map_or(0, |input| {
        input
            .position()
            .saturating_sub(recorder.base.load(Ordering::Relaxed))
    });
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *position = millis(frames, recorder.config.rate) };
    SUCCESS
}

/// Record events, markers, position updates and duration limits are accepted and never sent.
unsafe extern "C" fn record_register(
    interface: *const c_void,
    callback: *mut c_void,
    _context: *mut c_void,
) -> Result {
    if recorded(interface).is_none() {
        return PARAMETER_INVALID;
    }
    if !callback.is_null() {
        unimplemented_once("OpenSL ES record events (SLRecordItf callbacks)");
    }
    SUCCESS
}

unsafe extern "C" fn record_events(interface: *const c_void, _value: u32) -> Result {
    if recorded(interface).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn record_clear_marker(interface: *const c_void) -> Result {
    if recorded(interface).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn record_get_u32(interface: *const c_void, value: *mut u32) -> Result {
    if recorded(interface).is_none() || value.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *value = 0 };
    SUCCESS
}

/// Whether the app may record: the manifest must request `RECORD_AUDIO`.
fn may_record() -> bool {
    crate::aaudio::may_record()
}

/// `CreateAudioRecorder`: the default input device into a buffer queue of PCM.
unsafe extern "C" fn create_audio_recorder(
    engine: *const c_void,
    out: *mut Object,
    source: *const DataSource,
    sink: *const DataSink,
    interface_count: u32,
    interfaces: *const Id,
    required: *const u32,
) -> Result {
    if engine_of(engine).is_none() || out.is_null() || source.is_null() || sink.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: the API gives a source and sink of tagged locators and formats, and both arrays with
    // `interface_count` entries.
    let (length, (format, rate, channels)) = unsafe {
        let checked = check_required::<Recorder>(interface_count, interfaces, required);
        if checked != SUCCESS {
            return checked;
        }
        let device = (*source).locator;
        if device.is_null() || *device != LOCATOR_IODEVICE || *device.add(1) != IODEVICE_AUDIOINPUT
        {
            return PARAMETER_INVALID;
        }
        let length = match queue_length((*sink).locator) {
            Ok(length) => length,
            Err(error) => return error,
        };
        match pcm((*sink).format) {
            Ok(shape) => (length, shape),
            Err(error) => return error,
        }
    };
    if !may_record() {
        eprintln!(
            "uxdroid: OpenSL ES recorder refused: the app does not request android.permission.RECORD_AUDIO"
        );
        return PERMISSION_DENIED;
    }
    let mut config = Config {
        rate,
        channels,
        capacity: 0,
    };
    if config.validate().is_err() {
        return PARAMETER_INVALID;
    }
    config.capacity = config.period();
    let recorder = Arc::new(Recorder {
        object: &RECORDER_OBJECT,
        record: &RECORD_METHODS,
        queue: &QUEUE_METHODS,
        configuration: &CONFIGURATION_METHODS,
        state: AtomicU32::new(STATE_UNREALIZED),
        fifo: Fifo::new(length, format, channels),
        config,
        input: Mutex::new(None),
        record_state: AtomicU32::new(RECORDSTATE_STOPPED),
        base: AtomicU64::new(0),
    });
    recorder.fifo.interface.store(
        std::ptr::from_ref(&recorder.queue) as usize,
        Ordering::Relaxed,
    );
    let pointer = RECORDERS.add(recorder);
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *out = pointer.cast_const().cast() };
    SUCCESS
}

// The signatures of what crosses into app code by pointer rather than by name.

macro_rules! method {
    ($name:literal, $signature:literal, $function:expr) => {
        Export::function($name, $signature, $function as *const ())
    };
}

/// Each interface's method table, and the app callbacks this library calls, with the signatures a
/// translator needs. They are not libraries: no app names them in `DT_NEEDED`.
pub fn interfaces() -> [&'static Table; 9] {
    static OBJECT: Table = Table {
        library: "SLObjectItf",
        exports: &[
            method!(c"Realize", "u32(ptr,u32)", realize::<Engine>),
            method!(c"Resume", "u32(ptr,u32)", resume::<Engine>),
            method!(c"GetState", "u32(ptr,ptr)", get_state::<Engine>),
            method!(c"GetInterface", "u32(ptr,ptr,ptr)", get_interface::<Engine>),
            method!(
                c"RegisterCallback",
                "u32(ptr,ptr,ptr)",
                register_callback::<Engine>
            ),
            method!(c"AbortAsyncOperation", "void(ptr)", abort_async_operation),
            method!(c"Destroy", "void(ptr)", destroy::<Engine>),
            method!(c"SetPriority", "u32(ptr,i32,u32)", set_priority),
            method!(c"GetPriority", "u32(ptr,ptr,ptr)", get_priority),
            method!(
                c"SetLossOfControlInterfaces",
                "u32(ptr,i16,ptr,u32)",
                set_loss_of_control_interfaces
            ),
        ],
    };
    // The unsupported methods ignore their arguments; their signatures are the API's.
    static ENGINE: Table = Table {
        library: "SLEngineItf",
        exports: &[
            method!(
                c"CreateLEDDevice",
                "u32(ptr,ptr,u32,u32,ptr,ptr)",
                unsupported
            ),
            method!(
                c"CreateVibraDevice",
                "u32(ptr,ptr,u32,u32,ptr,ptr)",
                unsupported
            ),
            method!(
                c"CreateAudioPlayer",
                "u32(ptr,ptr,ptr,ptr,u32,ptr,ptr)",
                create_audio_player
            ),
            method!(
                c"CreateAudioRecorder",
                "u32(ptr,ptr,ptr,ptr,u32,ptr,ptr)",
                create_audio_recorder
            ),
            method!(
                c"CreateMidiPlayer",
                "u32(ptr,ptr,ptr,ptr,ptr,ptr,ptr,u32,ptr,ptr)",
                unsupported
            ),
            method!(c"CreateListener", "u32(ptr,ptr,u32,ptr,ptr)", unsupported),
            method!(c"Create3DGroup", "u32(ptr,ptr,u32,ptr,ptr)", unsupported),
            method!(
                c"CreateOutputMix",
                "u32(ptr,ptr,u32,ptr,ptr)",
                create_output_mix
            ),
            method!(
                c"CreateMetadataExtractor",
                "u32(ptr,ptr,ptr,u32,ptr,ptr)",
                unsupported
            ),
            method!(
                c"CreateExtensionObject",
                "u32(ptr,ptr,ptr,u32,u32,ptr,ptr)",
                unsupported
            ),
            method!(
                c"QueryNumSupportedInterfaces",
                "u32(ptr,u32,ptr)",
                unsupported
            ),
            method!(
                c"QuerySupportedInterfaces",
                "u32(ptr,u32,u32,ptr)",
                unsupported
            ),
            method!(c"QueryNumSupportedExtensions", "u32(ptr,ptr)", unsupported),
            method!(
                c"QuerySupportedExtension",
                "u32(ptr,u32,ptr,ptr)",
                unsupported
            ),
            method!(c"IsExtensionSupported", "u32(ptr,ptr,ptr)", unsupported),
        ],
    };
    static OUTPUT_MIX: Table = Table {
        library: "SLOutputMixItf",
        exports: &[
            method!(
                c"GetDestinationOutputDeviceIDs",
                "u32(ptr,ptr,ptr)",
                mix_devices
            ),
            method!(
                c"RegisterDeviceChangeCallback",
                "u32(ptr,ptr,ptr)",
                mix_register
            ),
            method!(c"ReRoute", "u32(ptr,i32,ptr)", mix_re_route),
        ],
    };
    static QUEUE: Table = Table {
        library: "SLAndroidSimpleBufferQueueItf",
        exports: &[
            method!(c"Enqueue", "u32(ptr,ptr,u32)", enqueue),
            method!(c"Clear", "u32(ptr)", clear),
            method!(c"GetState", "u32(ptr,ptr)", queue_state),
            method!(c"RegisterCallback", "u32(ptr,ptr,ptr)", queue_register),
        ],
    };
    static PLAY: Table = Table {
        library: "SLPlayItf",
        exports: &[
            method!(c"SetPlayState", "u32(ptr,u32)", set_play_state),
            method!(c"GetPlayState", "u32(ptr,ptr)", get_play_state),
            method!(c"GetDuration", "u32(ptr,ptr)", get_duration),
            method!(c"GetPosition", "u32(ptr,ptr)", play_position),
            method!(c"RegisterCallback", "u32(ptr,ptr,ptr)", play_register),
            method!(c"SetCallbackEventsMask", "u32(ptr,u32)", play_events),
            method!(c"GetCallbackEventsMask", "u32(ptr,ptr)", play_get_u32),
            method!(c"SetMarkerPosition", "u32(ptr,u32)", play_events),
            method!(c"ClearMarkerPosition", "u32(ptr)", play_clear_marker),
            method!(c"GetMarkerPosition", "u32(ptr,ptr)", play_get_u32),
            method!(c"SetPositionUpdatePeriod", "u32(ptr,u32)", play_events),
            method!(c"GetPositionUpdatePeriod", "u32(ptr,ptr)", play_get_u32),
        ],
    };
    static VOLUME: Table = Table {
        library: "SLVolumeItf",
        exports: &[
            method!(c"SetVolumeLevel", "u32(ptr,i16)", set_volume_level),
            method!(c"GetVolumeLevel", "u32(ptr,ptr)", get_volume_level),
            method!(c"GetMaxVolumeLevel", "u32(ptr,ptr)", get_max_volume_level),
            method!(c"SetMute", "u32(ptr,u32)", set_mute),
            method!(c"GetMute", "u32(ptr,ptr)", get_mute),
            method!(
                c"EnableStereoPosition",
                "u32(ptr,u32)",
                enable_stereo_position
            ),
            method!(
                c"IsEnabledStereoPosition",
                "u32(ptr,ptr)",
                is_enabled_stereo_position
            ),
            method!(c"SetStereoPosition", "u32(ptr,i16)", set_stereo_position),
            method!(c"GetStereoPosition", "u32(ptr,ptr)", get_stereo_position),
        ],
    };
    static RECORD: Table = Table {
        library: "SLRecordItf",
        exports: &[
            method!(c"SetRecordState", "u32(ptr,u32)", set_record_state),
            method!(c"GetRecordState", "u32(ptr,ptr)", get_record_state),
            method!(c"SetDurationLimit", "u32(ptr,u32)", record_events),
            method!(c"GetPosition", "u32(ptr,ptr)", record_position),
            method!(c"RegisterCallback", "u32(ptr,ptr,ptr)", record_register),
            method!(c"SetCallbackEventsMask", "u32(ptr,u32)", record_events),
            method!(c"GetCallbackEventsMask", "u32(ptr,ptr)", record_get_u32),
            method!(c"SetMarkerPosition", "u32(ptr,u32)", record_events),
            method!(c"ClearMarkerPosition", "u32(ptr)", record_clear_marker),
            method!(c"GetMarkerPosition", "u32(ptr,ptr)", record_get_u32),
            method!(c"SetPositionUpdatePeriod", "u32(ptr,u32)", record_events),
            method!(c"GetPositionUpdatePeriod", "u32(ptr,ptr)", record_get_u32),
        ],
    };
    static CONFIGURATION: Table = Table {
        library: "SLAndroidConfigurationItf",
        exports: &[
            method!(
                c"SetConfiguration",
                "u32(ptr,ptr,ptr,u32)",
                configuration_set
            ),
            method!(
                c"GetConfiguration",
                "u32(ptr,ptr,ptr,ptr)",
                configuration_get
            ),
            method!(c"AcquireJavaProxy", "u32(ptr,u32,ptr)", java_proxy),
            method!(c"ReleaseJavaProxy", "u32(ptr,u32)", release_java_proxy),
        ],
    };
    /// Called by this library, into app code: their addresses are the app's, so none is bound here.
    static CALLBACKS: Table = Table {
        library: "callbacks",
        exports: &[
            Export::function(
                c"slAndroidSimpleBufferQueueCallback",
                "void(ptr,ptr)",
                std::ptr::null(),
            ),
            Export::function(
                c"AAudioStream_dataCallback",
                "i32(ptr,ptr,ptr,i32)",
                std::ptr::null(),
            ),
            Export::function(
                c"AAudioStream_errorCallback",
                "void(ptr,ptr,i32)",
                std::ptr::null(),
            ),
        ],
    };
    [
        &OBJECT,
        &ENGINE,
        &OUTPUT_MIX,
        &QUEUE,
        &PLAY,
        &VOLUME,
        &RECORD,
        &CONFIGURATION,
        &CALLBACKS,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A realized engine and its `SLEngineItf`.
    fn engine() -> (Object, *const *const EngineMethods) {
        let mut object: Object = std::ptr::null();
        let mut engine: *const *const EngineMethods = std::ptr::null();
        // SAFETY: valid outputs and no requested interfaces.
        unsafe {
            assert_eq!(
                create_engine(
                    &mut object,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null()
                ),
                SUCCESS
            );
            let methods = &**object;
            assert_eq!(
                (methods.get_interface)(object, ids::SL_IID_ENGINE.0, (&raw mut engine).cast()),
                PRECONDITIONS_VIOLATED
            );
            assert_eq!((methods.realize)(object, 0), SUCCESS);
            assert_eq!((methods.realize)(object, 0), PRECONDITIONS_VIOLATED);
            assert_eq!(
                (methods.get_interface)(object, ids::SL_IID_ENGINE.0, (&raw mut engine).cast()),
                SUCCESS
            );
        }
        (object, engine)
    }

    fn realize(object: Object) {
        // SAFETY: a live object.
        unsafe { assert_eq!(((**object).realize)(object, 0), SUCCESS) };
    }

    fn interface<T>(object: Object, id: &InterfaceId) -> *const *const T {
        let mut out: *const *const T = std::ptr::null();
        // SAFETY: a live, realized object.
        let result = unsafe { ((**object).get_interface)(object, id.0, (&raw mut out).cast()) };
        assert_eq!(result, SUCCESS);
        out
    }

    fn destroy(object: Object) {
        // SAFETY: a live object, destroyed once.
        unsafe { ((**object).destroy)(object) };
    }

    fn output_mix(engine: *const *const EngineMethods) -> Object {
        let mut mix: Object = std::ptr::null();
        // SAFETY: a live engine interface.
        unsafe {
            assert_eq!(
                ((**engine).create_output_mix)(
                    engine.cast(),
                    &mut mix,
                    0,
                    std::ptr::null(),
                    std::ptr::null()
                ),
                SUCCESS
            );
        }
        realize(mix);
        mix
    }

    const STEREO_16: [u32; 7] = [
        DATAFORMAT_PCM,
        2,
        8_000_000,
        16,
        16,
        3,
        BYTEORDER_LITTLEENDIAN,
    ];
    const MONO_16: [u32; 7] = [
        DATAFORMAT_PCM,
        1,
        8_000_000,
        16,
        16,
        4,
        BYTEORDER_LITTLEENDIAN,
    ];

    fn player(
        engine: *const *const EngineMethods,
        mix: Object,
        locator: &[u32],
        format: *const u32,
        ids: &[Id],
    ) -> std::result::Result<Object, Result> {
        let mix_locator = [LOCATOR_OUTPUTMIX as usize, mix as usize];
        let source = DataSource {
            locator: locator.as_ptr(),
            format,
        };
        let sink = DataSource {
            locator: mix_locator.as_ptr().cast(),
            format: std::ptr::null(),
        };
        let required = vec![1u32; ids.len()];
        let mut player: Object = std::ptr::null();
        // SAFETY: a live engine, valid descriptions and arrays of `ids.len()` entries.
        let result = unsafe {
            ((**engine).create_audio_player)(
                engine.cast(),
                &mut player,
                &source,
                &sink,
                ids.len() as u32,
                ids.as_ptr(),
                required.as_ptr(),
            )
        };
        if result == SUCCESS {
            Ok(player)
        } else {
            Err(result)
        }
    }

    #[test]
    fn the_engine_makes_mixes_and_refuses_other_devices() {
        let (object, engine) = engine();
        let mix = output_mix(engine);
        let mixer = interface::<MixMethods>(mix, &ids::SL_IID_OUTPUTMIX);
        let (mut count, mut device) = (4, 0);
        // SAFETY: live interfaces; the unsupported methods ignore their arguments.
        unsafe {
            assert_eq!(
                ((**mixer).get_destination_output_device_ids)(
                    mixer.cast(),
                    &mut count,
                    &mut device
                ),
                SUCCESS
            );
            assert_eq!((count, device), (1, DEFAULT_OUTPUT_DEVICE));
            assert_eq!(((**engine).create_led_device)(), FEATURE_UNSUPPORTED);
            let mut state = 0;
            assert_eq!(((**object).get_state)(object, &mut state), SUCCESS);
            assert_eq!(state, STATE_REALIZED);
        }
        destroy(mix);
        destroy(object);
        // SAFETY: the destroyed object's pointer must be refused, not dereferenced as live.
        unsafe { assert_eq!((ENGINE_OBJECT.realize)(object, 0), PARAMETER_INVALID) };
        assert!(output_mix_refused(engine));
    }

    fn output_mix_refused(engine: *const *const EngineMethods) -> bool {
        let mut mix: Object = std::ptr::null();
        // SAFETY: the engine interface of a destroyed engine is looked up, never used.
        let result = unsafe {
            (ENGINE_METHODS.create_output_mix)(
                engine.cast(),
                &mut mix,
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        result == PARAMETER_INVALID
    }

    struct Played {
        buffers: Vec<usize>,
        at: Vec<Instant>,
    }

    unsafe extern "C" fn on_played(queue: *const c_void, context: *mut c_void) {
        // SAFETY: the test's record, alive until the player is destroyed.
        let played = unsafe { &mut *context.cast::<Mutex<Played>>() };
        let mut played = lock(played);
        played.buffers.push(queue as usize);
        played.at.push(Instant::now());
    }

    #[test]
    fn a_player_plays_its_queue_in_order_and_calls_back_once_per_buffer() {
        let (object, engine) = engine();
        let mix = output_mix(engine);
        let queue_locator = [LOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 2];
        let player = player(
            engine,
            mix,
            &queue_locator,
            STEREO_16.as_ptr(),
            &[ids::SL_IID_BUFFERQUEUE.0],
        )
        .unwrap();
        realize(player);
        let play = interface::<PlayMethods>(player, &ids::SL_IID_PLAY);
        let queue = interface::<QueueMethods>(player, &ids::SL_IID_BUFFERQUEUE);
        assert_eq!(
            queue,
            interface(player, &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE)
        );
        let played = Mutex::new(Played {
            buffers: Vec::new(),
            at: Vec::new(),
        });
        // 80 stereo frames each: one 10 ms period at 8 kHz.
        let buffers = [[0x1234i16; 160]; 3];
        // SAFETY: live interfaces; the buffers and the record outlive the player.
        unsafe {
            let q = &**queue;
            assert_eq!(
                (q.register_callback)(
                    queue.cast(),
                    Some(on_played),
                    (&raw const played).cast_mut().cast()
                ),
                SUCCESS
            );
            assert_eq!(
                (q.enqueue)(queue.cast(), buffers[0].as_ptr().cast(), 320),
                SUCCESS
            );
            assert_eq!(
                (q.enqueue)(queue.cast(), buffers[1].as_ptr().cast(), 320),
                SUCCESS
            );
            assert_eq!(
                (q.enqueue)(queue.cast(), buffers[2].as_ptr().cast(), 320),
                BUFFER_INSUFFICIENT
            );
            assert_eq!(
                (q.enqueue)(queue.cast(), std::ptr::null(), 320),
                PARAMETER_INVALID
            );
            let mut state = [9u32; 2];
            assert_eq!((q.get_state)(queue.cast(), &mut state), SUCCESS);
            assert_eq!(state, [2, 0]);
            assert_eq!(
                ((**play).set_play_state)(play.cast(), PLAYSTATE_PLAYING),
                SUCCESS
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while lock(&played).buffers.len() < 2 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!((q.get_state)(queue.cast(), &mut state), SUCCESS);
            assert_eq!(state, [0, 2]);
            let mut playing = 0;
            assert_eq!(
                ((**play).get_play_state)(play.cast(), &mut playing),
                SUCCESS
            );
            assert_eq!(playing, PLAYSTATE_PLAYING);
            let mut position = 0;
            assert_eq!(((**play).get_position)(play.cast(), &mut position), SUCCESS);
            assert!(position >= 20, "{position} ms");
            assert_eq!(((**play).set_play_state)(play.cast(), 9), PARAMETER_INVALID);
        }
        let played = lock(&played);
        assert_eq!(
            played.buffers, [queue as usize; 2],
            "callbacks get the queue interface"
        );
        let gap = played.at[1] - played.at[0];
        assert!(
            gap >= Duration::from_millis(7) && gap < Duration::from_millis(40),
            "{gap:?}"
        );
        drop(played);
        destroy(player);
        // SAFETY: the destroyed player's interface must be refused.
        unsafe { assert_eq!((QUEUE_METHODS.clear)(queue.cast()), PARAMETER_INVALID) };
        destroy(mix);
        destroy(object);
    }

    #[test]
    fn the_queue_converts_each_format_and_hands_back_finished_buffers() {
        let fifo = Fifo::new(2, Format::I16, 2);
        let first = [16384i16, -16384, 8192, 0];
        let second = [32767i16, -32768];
        for buffer in [&first[..], &second[..]] {
            lock(&fifo.queue).buffers.push_back(Buffer {
                address: buffer.as_ptr() as usize,
                length: size_of_val(buffer),
                done: 0,
            });
        }
        let mut out = [9.0f32; 8];
        fifo.play(&mut out);
        assert_eq!(
            out,
            [0.5, -0.5, 0.25, 0.0, 32767.0 / 32768.0, -1.0, 0.0, 0.0]
        );
        assert_eq!(lock(&fifo.queue).completed, 2);

        let recording = Fifo::new(1, Format::U8, 1);
        let mut room = [0u8; 3];
        lock(&recording.queue).buffers.push_back(Buffer {
            address: room.as_mut_ptr() as usize,
            length: room.len(),
            done: 0,
        });
        recording.record(&[0.5, -1.0]);
        assert_eq!(
            lock(&recording.queue).completed,
            0,
            "one byte is still empty"
        );
        recording.record(&[0.0, 1.0]);
        assert_eq!(room, [192, 0, 128]);
        assert_eq!(lock(&recording.queue).completed, 1);
        assert!(
            lock(&recording.queue).buffers.is_empty(),
            "what finds no buffer is lost"
        );
    }

    struct Filled(Mutex<u32>);

    unsafe extern "C" fn on_filled(_queue: *const c_void, context: *mut c_void) {
        // SAFETY: the test's counter, alive until the recorder is destroyed.
        *lock(unsafe { &(*context.cast::<Filled>()).0 }) += 1;
    }

    #[test]
    fn a_recorder_fills_its_buffers_from_the_input_device() {
        let (object, engine) = engine();
        let device = [LOCATOR_IODEVICE, IODEVICE_AUDIOINPUT, 0xFFFF_FFFF, 0];
        let queue_locator = [LOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 1];
        let source = DataSource {
            locator: device.as_ptr(),
            format: std::ptr::null(),
        };
        let sink = DataSource {
            locator: queue_locator.as_ptr(),
            format: MONO_16.as_ptr(),
        };
        let ids = [
            ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE.0,
            ids::SL_IID_ANDROIDCONFIGURATION.0,
        ];
        let required = [1u32, 0];
        let mut recorder: Object = std::ptr::null();
        let filled = Filled(Mutex::new(0));
        let mut samples = [7i16; 160];
        // SAFETY: a live engine, valid descriptions; the buffer and counter outlive the recorder.
        unsafe {
            assert_eq!(
                ((**engine).create_audio_recorder)(
                    engine.cast(),
                    &mut recorder,
                    &source,
                    &sink,
                    2,
                    ids.as_ptr(),
                    required.as_ptr()
                ),
                SUCCESS
            );
            realize(recorder);
            let record = interface::<RecordMethods>(recorder, &ids::SL_IID_RECORD);
            let queue = interface::<QueueMethods>(recorder, &ids::SL_IID_ANDROIDSIMPLEBUFFERQUEUE);
            let configuration =
                interface::<ConfigurationMethods>(recorder, &ids::SL_IID_ANDROIDCONFIGURATION);
            let preset = 6u32;
            assert_eq!(
                ((**configuration).set)(
                    configuration.cast(),
                    c"androidRecordingPreset".as_ptr(),
                    (&raw const preset).cast(),
                    4
                ),
                SUCCESS
            );
            assert_eq!(
                ((**queue).register_callback)(
                    queue.cast(),
                    Some(on_filled),
                    (&raw const filled).cast_mut().cast()
                ),
                SUCCESS
            );
            assert_eq!(
                ((**queue).enqueue)(queue.cast(), samples.as_mut_ptr().cast(), 320),
                SUCCESS
            );
            assert_eq!(
                ((**record).set_record_state)(record.cast(), RECORDSTATE_RECORDING),
                SUCCESS
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while *lock(&filled.0) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                ((**record).set_record_state)(record.cast(), RECORDSTATE_STOPPED),
                SUCCESS
            );
        }
        assert_eq!(*lock(&filled.0), 1);
        assert!(
            samples.iter().all(|&s| s == 0),
            "the null device records silence"
        );
        destroy(recorder);
        destroy(object);
    }

    #[test]
    fn what_needs_a_decoder_or_is_malformed_is_refused() {
        let (object, engine) = engine();
        let mix = output_mix(engine);
        let queue_locator = [LOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 2];
        // SL_DATALOCATOR_URI, with an MP3's MIME format.
        let uri = [1u32, 0, 0];
        let mime = [1u32, 0, 0];
        assert_eq!(
            player(engine, mix, &uri, mime.as_ptr(), &[]),
            Err(CONTENT_UNSUPPORTED)
        );
        let big_endian = [DATAFORMAT_PCM, 2, 8_000_000, 16, 16, 3, 1];
        assert_eq!(
            player(engine, mix, &queue_locator, big_endian.as_ptr(), &[]),
            Err(CONTENT_UNSUPPORTED)
        );
        let float_ex = [
            ANDROID_DATAFORMAT_PCM_EX,
            1,
            48_000_000,
            32,
            32,
            4,
            BYTEORDER_LITTLEENDIAN,
            REPRESENTATION_FLOAT,
        ];
        assert!(player(engine, mix, &queue_locator, float_ex.as_ptr(), &[]).is_ok());
        let slow = [
            DATAFORMAT_PCM,
            1,
            1_000_000,
            16,
            16,
            4,
            BYTEORDER_LITTLEENDIAN,
        ];
        assert_eq!(
            player(engine, mix, &queue_locator, slow.as_ptr(), &[]),
            Err(PARAMETER_INVALID)
        );
        assert_eq!(
            player(
                engine,
                mix,
                &queue_locator,
                STEREO_16.as_ptr(),
                &[ids::SL_IID_SEEK.0]
            ),
            Err(FEATURE_UNSUPPORTED)
        );
        assert_eq!(
            player(
                engine,
                mix.wrapping_byte_add(8),
                &queue_locator,
                STEREO_16.as_ptr(),
                &[]
            ),
            Err(PARAMETER_INVALID),
            "a forged output mix"
        );
        let empty = [LOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 0];
        assert_eq!(
            player(engine, mix, &empty, STEREO_16.as_ptr(), &[]),
            Err(PARAMETER_INVALID)
        );
        destroy(mix);
        destroy(object);
    }

    #[test]
    fn volume_is_in_millibels_with_an_optional_stereo_position() {
        let level = |level| {
            Volume {
                level,
                ..Volume::default()
            }
            .gains()
        };
        assert_eq!(level(0), (1.0, 1.0));
        let (left, right) = level(-600);
        assert!((left - 0.501).abs() < 0.001 && left == right, "{left}");
        assert_eq!(
            Volume {
                mute: true,
                ..Volume::default()
            }
            .gains(),
            (0.0, 0.0)
        );
        let panned = Volume {
            stereo: true,
            position: 500,
            ..Volume::default()
        }
        .gains();
        assert_eq!(panned, (0.5, 1.0));
        let left_only = Volume {
            stereo: true,
            position: -1000,
            ..Volume::default()
        }
        .gains();
        assert_eq!(left_only, (1.0, 0.0));
    }

    #[test]
    fn a_queue_callback_changes_only_while_stopped_and_clear_restarts_the_index() {
        let (object, engine) = engine();
        let mix = output_mix(engine);
        let queue_locator = [LOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 2];
        let player = player(
            engine,
            mix,
            &queue_locator,
            STEREO_16.as_ptr(),
            &[ids::SL_IID_BUFFERQUEUE.0],
        )
        .unwrap();
        realize(player);
        let play = interface::<PlayMethods>(player, &ids::SL_IID_PLAY);
        let queue = interface::<QueueMethods>(player, &ids::SL_IID_BUFFERQUEUE);
        let played = Mutex::new(Played {
            buffers: Vec::new(),
            at: Vec::new(),
        });
        let buffer = [0x1234i16; 160];
        // SAFETY: live interfaces; the buffer and the record outlive the player.
        unsafe {
            let q = &**queue;
            let context = (&raw const played).cast_mut().cast();
            assert_eq!(
                (q.register_callback)(queue.cast(), Some(on_played), context),
                SUCCESS
            );
            assert_eq!(
                (q.enqueue)(queue.cast(), buffer.as_ptr().cast(), 320),
                SUCCESS
            );
            assert_eq!(
                ((**play).set_play_state)(play.cast(), PLAYSTATE_PLAYING),
                SUCCESS
            );
            // As Android: the callback the device thread calls changes only while stopped.
            assert_eq!(
                (q.register_callback)(queue.cast(), None, std::ptr::null_mut()),
                PRECONDITIONS_VIOLATED
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while lock(&played).buffers.is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                ((**play).set_play_state)(play.cast(), PLAYSTATE_STOPPED),
                SUCCESS
            );
            assert_eq!(
                (q.register_callback)(queue.cast(), None, std::ptr::null_mut()),
                SUCCESS
            );
            let mut state = [9u32; 2];
            assert_eq!((q.get_state)(queue.cast(), &mut state), SUCCESS);
            assert_eq!(state, [0, 1]);
            assert_eq!((q.clear)(queue.cast()), SUCCESS);
            assert_eq!((q.get_state)(queue.cast(), &mut state), SUCCESS);
            assert_eq!(
                state,
                [0, 0],
                "a cleared queue counts from its first buffer again"
            );
        }
        destroy(player);
        destroy(mix);
        destroy(object);
    }

    #[test]
    fn every_method_table_and_callback_has_a_valid_signature() {
        for table in interfaces() {
            uxdroid_bridge::table::validate(table).unwrap();
        }
        let engine = interfaces()[1];
        assert_eq!(
            engine.exports.len(),
            size_of::<EngineMethods>() / size_of::<usize>()
        );
        assert_eq!(
            interfaces()[0].exports.len(),
            size_of::<ObjectMethods>() / size_of::<usize>()
        );
        assert_eq!(
            interfaces()[4].exports.len(),
            size_of::<PlayMethods>() / size_of::<usize>()
        );
        assert_eq!(
            interfaces()[6].exports.len(),
            size_of::<RecordMethods>() / size_of::<usize>()
        );
        assert_eq!(
            interfaces()[5].exports.len(),
            size_of::<VolumeMethods>() / size_of::<usize>()
        );
    }
}
