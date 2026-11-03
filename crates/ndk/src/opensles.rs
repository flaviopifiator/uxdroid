//! `libOpenSLES`: an engine that realizes and then reports every audio object as unsupported.
//!
//! Apps such as RustDesk link the interface IDs and create the engine at start, and handle
//! `SL_RESULT_FEATURE_UNSUPPORTED` when a player or recorder cannot be made; audio over PipeWire
//! comes later. The interface IDs are objects, so the table binds them as data: each is a pointer
//! to a GUID of this library. App code compares the pointers, never the GUIDs, as the API intends.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use uxdroid_bridge::table::{Export, Table};

use crate::live::Live;

type Result = u32;

const SUCCESS: Result = 0;
const PRECONDITIONS_VIOLATED: Result = 1;
const PARAMETER_INVALID: Result = 2;
const FEATURE_UNSUPPORTED: Result = 12;
const STATE_UNREALIZED: u32 = 1;
const STATE_REALIZED: u32 = 2;

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
    SL_IID_PITCH 41, SL_IID_PLAY 42, SL_IID_PLAYBACKRATE 43, SL_IID_PREFETCHSTATUS 44,
    SL_IID_PRESETREVERB 45, SL_IID_RATEPITCH 46, SL_IID_RECORD 47, SL_IID_SEEK 48,
    SL_IID_THREADSYNC 49, SL_IID_VIBRA 50, SL_IID_VIRTUALIZER 51, SL_IID_VISUALIZATION 52,
    SL_IID_VOLUME 53,
}

/// `struct SLObjectItf_`, the methods of every OpenSL ES object.
#[repr(C)]
pub(crate) struct ObjectMethods {
    realize: unsafe extern "C" fn(*const *const ObjectMethods, u32) -> Result,
    resume: unsafe extern "C" fn(*const *const ObjectMethods, u32) -> Result,
    get_state: unsafe extern "C" fn(*const *const ObjectMethods, *mut u32) -> Result,
    get_interface:
        unsafe extern "C" fn(*const *const ObjectMethods, *const Guid, *mut c_void) -> Result,
    register_callback:
        unsafe extern "C" fn(*const *const ObjectMethods, *mut c_void, *mut c_void) -> Result,
    abort_async_operation: unsafe extern "C" fn(*const *const ObjectMethods),
    destroy: unsafe extern "C" fn(*const *const ObjectMethods),
    set_priority: unsafe extern "C" fn(*const *const ObjectMethods, i32, u32) -> Result,
    get_priority: unsafe extern "C" fn(*const *const ObjectMethods, *mut i32, *mut u32) -> Result,
    set_loss_of_control_interfaces:
        unsafe extern "C" fn(*const *const ObjectMethods, i16, *mut c_void, u32) -> Result,
}

/// `struct SLEngineItf_`: fifteen methods, which here all answer that the feature is unsupported.
/// None reads its arguments, which the System V convention leaves to the caller to clean up.
#[repr(C)]
struct EngineMethods([unsafe extern "C" fn() -> Result; 15]);

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

extern "C" fn unsupported() -> Result {
    FEATURE_UNSUPPORTED
}

static ENGINE_METHODS: EngineMethods = EngineMethods([unsupported; 15]);

static OBJECT_METHODS: ObjectMethods = ObjectMethods {
    realize,
    resume,
    get_state,
    get_interface,
    register_callback,
    abort_async_operation,
    destroy,
    set_priority,
    get_priority,
    set_loss_of_control_interfaces,
};

fn engine(object: *const *const ObjectMethods) -> Option<Arc<Engine>> {
    // The object interface is the engine's first field, so both share an address.
    ENGINES.get(object.cast())
}

unsafe extern "C" fn realize(object: *const *const ObjectMethods, _asynchronous: u32) -> Result {
    let Some(engine) = engine(object) else {
        return PARAMETER_INVALID;
    };
    match engine.state.compare_exchange(
        STATE_UNREALIZED,
        STATE_REALIZED,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => SUCCESS,
        Err(_) => PRECONDITIONS_VIOLATED,
    }
}

unsafe extern "C" fn resume(object: *const *const ObjectMethods, _asynchronous: u32) -> Result {
    if engine(object).is_some() {
        PRECONDITIONS_VIOLATED
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn get_state(object: *const *const ObjectMethods, state: *mut u32) -> Result {
    let Some(engine) = engine(object) else {
        return PARAMETER_INVALID;
    };
    if state.is_null() {
        return PARAMETER_INVALID;
    }
    // SAFETY: checked non-null; the API makes it writable.
    unsafe { *state = engine.state.load(Ordering::Acquire) };
    SUCCESS
}

unsafe extern "C" fn get_interface(
    object: *const *const ObjectMethods,
    id: *const Guid,
    out: *mut c_void,
) -> Result {
    let Some(engine) = engine(object) else {
        return PARAMETER_INVALID;
    };
    if out.is_null() {
        return PARAMETER_INVALID;
    }
    if engine.state.load(Ordering::Acquire) != STATE_REALIZED {
        return PRECONDITIONS_VIOLATED;
    }
    let interface: *const c_void = if id == ids::SL_IID_ENGINE.0 {
        std::ptr::from_ref(&engine.engine).cast()
    } else if id == ids::SL_IID_OBJECT.0 {
        std::ptr::from_ref(&engine.object).cast()
    } else {
        return FEATURE_UNSUPPORTED;
    };
    // SAFETY: the API makes `out` a writable interface pointer; the engine lives until Destroy.
    unsafe { out.cast::<*const c_void>().write(interface) };
    SUCCESS
}

unsafe extern "C" fn register_callback(
    object: *const *const ObjectMethods,
    _callback: *mut c_void,
    _context: *mut c_void,
) -> Result {
    if engine(object).is_some() {
        SUCCESS
    } else {
        PARAMETER_INVALID
    }
}

unsafe extern "C" fn abort_async_operation(_object: *const *const ObjectMethods) {}

unsafe extern "C" fn destroy(object: *const *const ObjectMethods) {
    drop(ENGINES.remove(object.cast()));
}

unsafe extern "C" fn set_priority(
    _object: *const *const ObjectMethods,
    _priority: i32,
    _preemptable: u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn get_priority(
    _object: *const *const ObjectMethods,
    _priority: *mut i32,
    _preemptable: *mut u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

unsafe extern "C" fn set_loss_of_control_interfaces(
    _object: *const *const ObjectMethods,
    _count: i16,
    _ids: *mut c_void,
    _enabled: u32,
) -> Result {
    FEATURE_UNSUPPORTED
}

/// `SLresult slCreateEngine(SLObjectItf* pEngine, SLuint32 numOptions, const SLEngineOption*,
/// SLuint32 numInterfaces, const SLInterfaceID*, const SLboolean* pInterfaceRequired)`.
pub(crate) unsafe extern "C" fn create_engine(
    out: *mut *const *const ObjectMethods,
    _option_count: u32,
    _options: *const c_void,
    interface_count: u32,
    interfaces: *const *const Guid,
    required: *const u32,
) -> Result {
    if out.is_null() || (interface_count > 0 && (interfaces.is_null() || required.is_null())) {
        return PARAMETER_INVALID;
    }
    // Every requested interface counts; a list far longer than the API has interfaces is malformed.
    if interface_count > 1024 {
        return PARAMETER_INVALID;
    }
    for index in 0..interface_count as usize {
        // SAFETY: the API gives both arrays `interface_count` entries; both were checked non-null.
        let (id, needed) = unsafe { (*interfaces.add(index), *required.add(index)) };
        if needed != 0 && id != ids::SL_IID_ENGINE.0 && id != ids::SL_IID_OBJECT.0 {
            return FEATURE_UNSUPPORTED;
        }
    }
    let engine = Arc::new(Engine {
        object: &OBJECT_METHODS,
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
pub(crate) unsafe extern "C" fn query_interface(index: u32, id: *mut *const Guid) -> Result {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_realizes_and_its_objects_are_unsupported() {
        let mut object: *const *const ObjectMethods = std::ptr::null();
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
            let mut engine: *const *const EngineMethods = std::ptr::null();
            assert_eq!(
                (methods.get_interface)(object, ids::SL_IID_ENGINE.0, (&raw mut engine).cast()),
                PRECONDITIONS_VIOLATED
            );
            assert_eq!((methods.realize)(object, 0), SUCCESS);
            let mut state = 0;
            assert_eq!((methods.get_state)(object, &mut state), SUCCESS);
            assert_eq!(state, STATE_REALIZED);
            assert_eq!(
                (methods.get_interface)(object, ids::SL_IID_ENGINE.0, (&raw mut engine).cast()),
                SUCCESS
            );
            // CreateOutputMix is the eighth method.
            assert_eq!(((**engine).0[7])(), FEATURE_UNSUPPORTED);
            assert_eq!(
                (methods.get_interface)(object, ids::SL_IID_PLAY.0, (&raw mut engine).cast()),
                FEATURE_UNSUPPORTED
            );
            (methods.destroy)(object);
            assert_eq!((methods.realize)(object, 0), PARAMETER_INVALID);
        }
    }

    #[test]
    fn required_audio_interfaces_refuse_the_engine() {
        let mut object: *const *const ObjectMethods = std::ptr::null();
        let interfaces = [ids::SL_IID_PLAY.0];
        let required = [1u32];
        // SAFETY: arrays of one entry each.
        let result = unsafe {
            create_engine(
                &mut object,
                0,
                std::ptr::null(),
                1,
                interfaces.as_ptr(),
                required.as_ptr(),
            )
        };
        assert_eq!(result, FEATURE_UNSUPPORTED);
    }

    #[test]
    fn a_required_interface_late_in_a_long_list_still_counts() {
        let mut object: *const *const ObjectMethods = std::ptr::null();
        let mut interfaces = vec![ids::SL_IID_ENGINE.0; 70];
        let mut required = vec![0u32; 70];
        interfaces[69] = ids::SL_IID_PLAY.0;
        required[69] = 1;
        // SAFETY: both arrays hold 70 entries.
        let result = unsafe {
            create_engine(
                &mut object,
                0,
                std::ptr::null(),
                70,
                interfaces.as_ptr(),
                required.as_ptr(),
            )
        };
        assert_eq!(result, FEATURE_UNSUPPORTED);
    }
}
