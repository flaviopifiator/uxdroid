//! Libraries for a guest ABI, `arm64-v8a`, run through a translation engine.
//!
//! The bridge keeps the guest ABI to itself: the loader maps and links a guest library as it does
//! a host one, and only the crossings differ.
//!
//! - Guest to host: every function a guest library imports from the host, from a signature table,
//!   the guest libc table or a JNI function table, becomes a *service*, a number with a two
//!   instruction guest stub, `svc #number; ret`. The engine reports the `svc`, and the service
//!   moves the arguments from AAPCS64 registers to System V ones by the function's signature and
//!   calls the host function.
//! - Host to guest: a guest function the host calls, such as a JNI method, a `pthread_create`
//!   start routine or a `qsort` comparator, gets a small host thunk that moves System V arguments
//!   into a guest CPU by its signature and runs the guest code until it returns to the return
//!   service.
//!
//! Guest memory is host memory: guest pointers are host pointers, so data crosses unchanged and only
//! registers move. Each host thread that runs guest code has a guest stack, a guest TLS block for
//! `TPIDR_EL0` and one CPU per nesting level, as a host call may call guest code again.

mod abi;
mod convert;
mod format;
mod jni;
mod jni_table;
mod libc;
mod libc_table;
mod native;
pub mod plan;
pub mod profile;

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::elf::ElfError;
use crate::table::{Signature, Table, Type};
use crate::unimplemented::Unimplemented;
use abi::{Class, GuestVaList, Slot, guest_layout, host_layout, normalize};

pub use jni::{method_trampoline as jni_method_trampoline, trampoline as jni_trampoline};

/// One guest CPU, used by one host thread. Methods take `&self` because the engine calls back
/// into the bridge while it runs, and the service then reads and writes the same CPU.
pub trait Cpu {
    /// x0 to x30.
    fn x(&self, index: usize) -> u64;
    fn set_x(&self, index: usize, value: u64);
    fn sp(&self) -> u64;
    fn set_sp(&self, value: u64);
    fn pc(&self) -> u64;
    fn set_pc(&self, value: u64);
    /// v0 to v31, low half first.
    fn v(&self, index: usize) -> [u64; 2];
    fn set_v(&self, index: usize, value: [u64; 2]);
    /// Runs from the PC until a service asks to stop. An error names an instruction the engine
    /// could not run.
    fn run(&self) -> Result<(), String>;
}

/// Called by the engine for every `svc #number`; `true` ends the current run.
pub type Supervisor = fn(&dyn Cpu, u32) -> bool;

/// A translator of guest code, such as Dynarmic for A64.
pub trait Engine: Send + Sync {
    fn name(&self) -> &str;

    /// A new CPU for the calling thread, whose `TPIDR_EL0` reads and writes `*tpidr_el0`.
    ///
    /// # Safety
    /// `tpidr_el0` must stay valid for the life of the CPU.
    unsafe fn cpu(
        &self,
        tpidr_el0: *mut u64,
        supervisor: Supervisor,
    ) -> Result<Box<dyn Cpu>, String>;

    /// The blocks the engine translated, oldest first, as locations: the guest PC in the low
    /// [`profile::PC_BITS`] bits and the engine's own flags for the block above them. Engines that
    /// keep no such list return none.
    fn translated(&self) -> Vec<u64> {
        Vec::new()
    }

    /// Translates the blocks at `locations`, as [`Engine::translated`] gives them, before any CPU
    /// runs them, on the calling thread. Returns how many it went through.
    ///
    /// # Safety
    /// The guest code at each location must be mapped and readable.
    unsafe fn pretranslate(&self, locations: &[u64]) -> usize {
        let _ = locations;
        0
    }

    /// Changes whenever the engine's locations or translation could: persisted lists of locations
    /// made by another engine are not used.
    fn identity(&self) -> u64 {
        0
    }
}

type Loader = fn() -> Result<Box<dyn Engine>, String>;

static LOADER: OnceLock<Loader> = OnceLock::new();
static ENGINE: EngineSlot = EngineSlot::new();

/// Names how to get the engine, which is loaded the first time a guest library loads, so a
/// process with host libraries only never pays for it.
pub fn set_engine_loader(loader: Loader) {
    let _ = LOADER.set(loader);
}

pub(crate) fn engine() -> Result<&'static dyn Engine, String> {
    ENGINE.get(LOADER.get().copied())
}

/// The engine once loaded. Only a success is kept: a failed load, such as a translator file that
/// was missing, is tried again by the next guest library.
struct EngineSlot {
    engine: OnceLock<Box<dyn Engine>>,
    loading: Mutex<()>,
}

impl EngineSlot {
    const fn new() -> EngineSlot {
        EngineSlot {
            engine: OnceLock::new(),
            loading: Mutex::new(()),
        }
    }

    fn get(&'static self, loader: Option<Loader>) -> Result<&'static dyn Engine, String> {
        if let Some(engine) = self.engine.get() {
            return Ok(engine.as_ref());
        }
        let _loading = self
            .loading
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(engine) = self.engine.get() {
            return Ok(engine.as_ref());
        }
        let loader = loader.ok_or("no translator is installed in this process")?;
        let engine = loader()?;
        Ok(self.engine.get_or_init(|| engine).as_ref())
    }
}

// --- Services: guest calls into the host ---

/// `svc` takes a 16-bit number.
const MAX_SERVICES: usize = 1 << 16;
/// `svc #0` is how arm64 Linux code makes a system call, with its number in x8, so it is never a
/// service: guest code that calls the kernel directly, as Go and hand-written assembly do, gets
/// the system call.
const SYSCALL_SERVICE: u32 = 0;
/// The return service takes the last number, which `add_service` never hands out.
const RETURN_SERVICE: u32 = (MAX_SERVICES - 1) as u32;

/// How the arguments after a variadic function's fixed ones are found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variadic {
    No,
    /// The last fixed argument is a printf format, which names them.
    Format,
    /// A `v` function of the printf family served by its variadic twin: the guest passes its
    /// fixed arguments and a `va_list`, from which the arguments its format names are read.
    FormatVaList,
    /// The last fixed argument is a scanf format: one pointer per conversion it names.
    Scan,
    /// A `v` function of the scanf family served by its variadic twin.
    ScanVaList,
    /// Integers only, as for `prctl`'s or `execl`'s: every guest integer register left is
    /// passed on, and the callee reads what it needs.
    Integers,
}

/// Where a host function is.
#[derive(Clone, Copy, Debug)]
enum Target {
    Fixed(usize),
    /// Slot n of the function table of the host `JNIEnv` the guest env's first argument stands for.
    JniEnv(usize),
    /// Slot n of the host `JavaVM`'s invoke interface.
    JavaVm(usize),
}

struct HostFunction {
    name: String,
    target: Target,
    returns: Type,
    parameters: Vec<Type>,
    variadic: Variadic,
    /// Parameters that are guest functions the host will call, with their signatures.
    callbacks: &'static [(usize, &'static str)],
    /// The first parameter is a `JNIEnv*`: guest code passes its guest env, the host needs its own.
    takes_env: bool,
}

/// NDK functions whose first parameter is a `JNIEnv*`, from the NDK's headers. The tables declare
/// it as a plain pointer; the guest's env there becomes the host env it stands for.
const ENV_FUNCTIONS: &[&str] = &[
    "AAssetManager_fromJava",
    "AFileDescriptor_create",
    "AFileDescriptor_getFd",
    "AFileDescriptor_setFd",
    "AHardwareBuffer_fromHardwareBuffer",
    "AHardwareBuffer_toHardwareBuffer",
    "AIBinder_fromJavaBinder",
    "AIBinder_toJavaBinder",
    "AInputQueue_fromJava",
    "AKeyEvent_fromJava",
    "AMidiDevice_fromJava",
    "AMotionEvent_fromJava",
    "ANativeWindow_fromSurface",
    "ANativeWindow_toSurface",
    "APerformanceHint_borrowSessionFromJava",
    "AParcel_fromJavaParcel",
    "ASharedMemory_dupFromJava",
    "ASurfaceControl_fromJava",
    "ASurfaceTexture_fromSurfaceTexture",
    "ASurfaceTransaction_fromJava",
    "AndroidBitmap_getDataSpace",
    "AndroidBitmap_getHardwareBuffer",
    "AndroidBitmap_getInfo",
    "AndroidBitmap_lockPixels",
    "AndroidBitmap_unlockPixels",
];

/// NDK functions that take a callback, which the tables declare as a plain pointer: the index of
/// that parameter and the callback's signature. A guest function there becomes a host thunk.
const CALLBACK_PARAMETERS: &[(&str, &[(usize, &str)])] = &[
    (
        "AAudioStreamBuilder_setDataCallback",
        &[(1, "i32(ptr,ptr,ptr,i32)")],
    ),
    (
        "AAudioStreamBuilder_setErrorCallback",
        &[(1, "void(ptr,ptr,i32)")],
    ),
    ("AChoreographer_postFrameCallback", &[(1, "void(i64,ptr)")]),
    (
        "AChoreographer_postFrameCallback64",
        &[(1, "void(i64,ptr)")],
    ),
    (
        "AChoreographer_postFrameCallbackDelayed",
        &[(1, "void(i64,ptr)")],
    ),
    (
        "AChoreographer_postFrameCallbackDelayed64",
        &[(1, "void(i64,ptr)")],
    ),
    ("ALooper_addFd", &[(4, "i32(i32,i32,ptr)")]),
];

fn callback_parameters(name: &str) -> &'static [(usize, &'static str)] {
    CALLBACK_PARAMETERS
        .iter()
        .find(|(known, _)| *known == name)
        .map_or(&[], |&(_, parameters)| parameters)
}

enum Service {
    /// Ends the run of the CPU it is called on: the guest function returned to the host.
    Return,
    Host(HostFunction),
    /// Reads its own arguments and writes its result, for calls that need more than a signature.
    Special(fn(&dyn Cpu)),
    /// A JNI `Call*Method`, `Call*MethodV` or `NewObject(V)`, served by its `A` form.
    JniCall(jni::MethodCall),
    /// A function the host has but guest code cannot call yet; stops the program naming it.
    Missing(String),
    /// A function of an NDK library uxdroid does not have yet; stops the program naming both.
    Unimplemented(Unimplemented),
}

static SERVICES: [AtomicPtr<Service>; MAX_SERVICES] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; MAX_SERVICES];
static NEXT_SERVICE: AtomicUsize = AtomicUsize::new(1);

fn add_service(service: Service) -> Result<u64, ElfError> {
    let number = NEXT_SERVICE.fetch_add(1, Ordering::Relaxed);
    if number >= RETURN_SERVICE as usize {
        return Err(ElfError::Unsupported("too many guest imports"));
    }
    service_at(number as u32, service)
}

fn service_at(number: u32, service: Service) -> Result<u64, ElfError> {
    let stub = native::stub(number).ok_or(ElfError::Unsupported("cannot map guest stubs"))?;
    SERVICES[number as usize].store(Box::into_raw(Box::new(service)), Ordering::Release);
    Ok(stub)
}

fn return_address() -> Result<u64, String> {
    static RETURN: OnceLock<Option<u64>> = OnceLock::new();
    RETURN
        .get_or_init(|| service_at(RETURN_SERVICE, Service::Return).ok())
        .ok_or_else(|| "cannot map guest stubs".to_string())
}

thread_local! {
    /// How many of this thread's guest CPUs are inside a service, that is, in host code. A guest
    /// call made while fewer are than are running, from a signal handler, interrupted translated
    /// code whose stack pointer the engine may not have written back yet.
    static SERVING: Cell<usize> = const { Cell::new(0) };
}

/// Counts a service while it runs, also when it leaves by stopping the program.
struct Serving;

impl Serving {
    fn enter() -> Serving {
        SERVING.with(|serving| serving.set(serving.get() + 1));
        Serving
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        SERVING.with(|serving| serving.set(serving.get().saturating_sub(1)));
    }
}

fn supervisor(cpu: &dyn Cpu, number: u32) -> bool {
    let _serving = Serving::enter();
    if number == SYSCALL_SERVICE {
        libc::raw_syscall(cpu);
        return false;
    }
    let service = SERVICES
        .get(number as usize)
        .map_or(std::ptr::null_mut(), |slot| slot.load(Ordering::Acquire));
    if service.is_null() {
        fail(cpu, format_args!("svc #{number}, which is no service"));
    }
    // SAFETY: services are leaked boxes, never freed or changed once published.
    match unsafe { &*service } {
        Service::Return => true,
        Service::Host(function) => {
            call_host(cpu, function);
            false
        }
        Service::Special(special) => {
            special(cpu);
            false
        }
        Service::JniCall(call) => {
            jni::call_method(cpu, call);
            false
        }
        Service::Missing(name) => crate::shims::die(format_args!(
            "uxdroid: {} has no guest signature yet, so arm64 code cannot call it\n",
            name.escape_debug()
        )),
        Service::Unimplemented(function) => crate::unimplemented::stop(function),
    }
}

fn fail(cpu: &dyn Cpu, what: std::fmt::Arguments<'_>) -> ! {
    crate::shims::die(format_args!(
        "uxdroid: guest code at {}: {what}\n",
        crate::loader::describe_address(cpu.pc())
    ))
}

/// A function to bind for a guest library's import, a stub address, or `None` when unknown.
pub(crate) fn import(
    name: &[u8],
    tables: &[&'static Table],
    is_data: bool,
) -> Result<Option<u64>, ElfError> {
    static IMPORTS: Mutex<Option<HashMap<Vec<u8>, u64>>> = Mutex::new(None);
    let mut imports = IMPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let imports = imports.get_or_insert_with(HashMap::new);
    if let Some(&address) = imports.get(name) {
        return Ok(Some(address));
    }
    let text = String::from_utf8_lossy(name).into_owned();
    let address = match resolve_import(&text, tables, is_data)? {
        Some(address) => address,
        None => return Ok(None),
    };
    imports.insert(name.to_vec(), address);
    Ok(Some(address))
}

fn resolve_import(
    name: &str,
    tables: &[&'static Table],
    is_data: bool,
) -> Result<Option<u64>, ElfError> {
    if let Some(twin) = libc::variadic_twin(name) {
        return va_list_service(name, twin, tables);
    }
    // Hands out function pointers, which must be guest stubs.
    if name == "eglGetProcAddress"
        && let Some(libc::Import::Special(special)) = libc::find(name)
    {
        return add_service(Service::Special(special)).map(Some);
    }
    for table in tables {
        if let Some(export) = table.find(name.as_bytes()) {
            let Some(address) = export.bind() else {
                return Ok(None);
            };
            if export.is_data() {
                // Guest pointers are host pointers, so a host object is the guest's too.
                return Ok(Some(address as u64));
            }
            return host_service(name, Target::Fixed(address), export.signature).map(Some);
        }
    }
    match libc::find(name) {
        Some(libc::Import::Function(signature)) => {
            let Some(address) = host_address(name, false) else {
                return Ok(None);
            };
            host_service(name, Target::Fixed(address), signature).map(Some)
        }
        Some(libc::Import::Special(special)) => add_service(Service::Special(special)).map(Some),
        Some(libc::Import::Data) => Ok(host_address(name, true).map(|address| address as u64)),
        None if is_data => Ok(None),
        // Only whether the host has it: taking an unaudited function's stub here would waste one.
        None if crate::shims::has_function(name) => {
            record_missing(name);
            add_service(Service::Missing(name.to_string())).map(Some)
        }
        None => Ok(None),
    }
}

/// The guest stub for an import of a function uxdroid does not have yet, one service per function.
pub(crate) fn unimplemented(function: Unimplemented) -> Result<u64, ElfError> {
    static STUBS: Mutex<Option<HashMap<(&'static str, &'static str), u64>>> = Mutex::new(None);
    let mut stubs = STUBS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stubs = stubs.get_or_insert_with(HashMap::new);
    let key = (function.library, function.name);
    if let Some(&stub) = stubs.get(&key) {
        return Ok(stub);
    }
    let stub = add_service(Service::Unimplemented(function))?;
    stubs.insert(key, stub);
    Ok(stub)
}

/// A `v` function served by its variadic twin, found the way an import of the twin would be.
fn va_list_service(
    name: &str,
    twin: &str,
    tables: &[&'static Table],
) -> Result<Option<u64>, ElfError> {
    let found = tables
        .iter()
        .find_map(|table| table.find(twin.as_bytes()))
        .map(|export| (export.bind(), export.signature));
    let (address, signature) = match found {
        Some((address, signature)) => (address, signature),
        None => match libc::find(twin) {
            Some(libc::Import::Function(signature)) => (host_address(twin, false), signature),
            _ => return Ok(None),
        },
    };
    let Some(address) = address else {
        return Ok(None);
    };
    let Ok(Signature::Function {
        returns,
        parameters,
        variadic: true,
    }) = Signature::parse(signature)
    else {
        return Err(ElfError::Unsupported("a va_list twin that is not variadic"));
    };
    add_service(Service::Host(HostFunction {
        name: name.to_string(),
        target: Target::Fixed(address),
        returns,
        parameters,
        variadic: match libc::is_scan_function(twin) {
            true => Variadic::ScanVaList,
            false => Variadic::FormatVaList,
        },
        callbacks: &[],
        takes_env: false,
    }))
    .map(Some)
}

fn host_address(name: &str, is_data: bool) -> Option<usize> {
    let name = std::ffi::CString::new(name).ok()?;
    crate::shims::resolve(&name, is_data)
}

fn host_service(name: &str, target: Target, signature: &str) -> Result<u64, ElfError> {
    let Ok(Signature::Function {
        returns,
        parameters,
        variadic,
    }) = Signature::parse(signature)
    else {
        return Err(ElfError::Unsupported("bad signature in a table"));
    };
    if parameters.contains(&Type::VaList) {
        record_missing(name);
        return add_service(Service::Missing(format!("{name} (a va_list)")));
    }
    let variadic = match variadic {
        false => Variadic::No,
        true if libc::is_format_function(name) => Variadic::Format,
        true if libc::is_scan_function(name) => Variadic::Scan,
        true => Variadic::Integers,
    };
    add_service(Service::Host(HostFunction {
        name: name.to_string(),
        target,
        returns,
        parameters,
        variadic,
        callbacks: callback_parameters(name),
        takes_env: ENV_FUNCTIONS.contains(&name),
    }))
}

static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record_missing(name: &str) {
    let mut missing = MISSING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !missing.iter().any(|known| known == name) {
        missing.push(name.to_string());
    }
}

/// How `name` binds for a guest library whose scope has `tables`, decided as `import` would but
/// without making a service, for install-time planning.
pub(crate) fn binding(name: &str, tables: &[&'static Table], is_data: bool) -> plan::Binding {
    use plan::Binding;
    if let Some(table) = tables
        .iter()
        .find(|table| table.find(name.as_bytes()).is_some())
    {
        return Binding::Table(table.library);
    }
    if libc::variadic_twin(name).is_some() {
        return Binding::Libc;
    }
    let unsigned_reason = || {
        libc_table::UNSIGNED
            .iter()
            .find(|(known, _)| *known == name)
            .map_or("no guest signature", |&(_, why)| why)
    };
    match libc::find(name) {
        Some(libc::Import::Function(_)) if crate::shims::is_audited(name) => Binding::Libc,
        Some(libc::Import::Function(_)) if crate::shims::has_function(name) => {
            Binding::Stub("the host function is not audited")
        }
        // The load finds no host function to call, as `resolve_import` does.
        Some(libc::Import::Function(_)) => Binding::Unresolved,
        Some(libc::Import::Special(_)) => Binding::Libc,
        Some(libc::Import::Data) if host_address(name, true).is_some() => Binding::Libc,
        Some(libc::Import::Data) => Binding::Unresolved,
        None if is_data => Binding::Unresolved,
        None if crate::shims::has_function(name) => Binding::Stub(unsigned_reason()),
        None => Binding::Unresolved,
    }
}

/// bionic's libc, libm and libdl exports that guest code cannot call yet, each with why, from the
/// NDK's arm64 libraries (`libc_table.py`) less the calls written by hand.
pub fn unsigned_bionic() -> Vec<(&'static str, &'static str)> {
    libc_table::UNSIGNED
        .iter()
        .filter(|(name, _)| libc::find(name).is_none() && libc::variadic_twin(name).is_none())
        .copied()
        .collect()
}

/// The imports bound so far that the host has but guest code cannot call yet, for reports.
pub fn missing_signatures() -> Vec<String> {
    MISSING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn read_slot(cpu: &dyn Cpu, slot: Slot) -> u64 {
    match slot {
        Slot::Integer(index) => cpu.x(index),
        Slot::Vector(index) => cpu.v(index)[0],
        // SAFETY: stack arguments are on the guest stack, which the caller wrote them to.
        Slot::Stack(index) => unsafe { native::read_u64(cpu.sp() + 8 * index as u64) },
    }
}

/// The classes of the arguments the printf or scanf format at `format` names, or the program
/// stops.
fn format_classes(cpu: &dyn Cpu, name: &str, format: u64, scan: bool) -> Vec<Class> {
    // SAFETY: the printf and scanf contracts make the format a C string; the read is bounded.
    let text = unsafe { native::c_string(format, format::MAX_FORMAT_BYTES + 1) };
    let classes = match scan {
        false => format::classes(text),
        true => format::scan_pointers(text).map(|count| vec![Class::Integer; count]),
    };
    match classes {
        Ok(extra) => extra,
        Err(error) => fail(cpu, format_args!("{name}: cannot carry format: {error:?}")),
    }
}

/// The value type the bridge reads for a variadic argument of `class`.
fn variadic_kind(class: Class) -> Type {
    match class {
        Class::Integer => Type::U64,
        Class::Vector => Type::F64,
    }
}

/// Reads a guest `va_list` the guest passed by address.
///
/// # Safety
/// `address` must be a guest `va_list`, 32 readable bytes.
unsafe fn read_va_list(address: u64) -> GuestVaList {
    let mut words = [0u64; 4];
    for (index, word) in words.iter_mut().enumerate() {
        // SAFETY: the caller's contract.
        *word = unsafe { native::read_u64(address + 8 * index as u64) };
    }
    GuestVaList::from_words(words)
}

/// The values of `classes`, read from a guest `va_list` at `address` in order.
fn va_arguments(cpu: &dyn Cpu, name: &str, address: u64, classes: &[Class]) -> Vec<u64> {
    if address == 0 {
        fail(cpu, format_args!("{name}: a null va_list"));
    }
    // SAFETY: the caller passes the guest's va_list argument; each address it yields is one the
    // guest's own va_arg would read.
    unsafe {
        let mut list = read_va_list(address);
        classes
            .iter()
            .map(|&class| native::read_u64(list.next(class)))
            .collect()
    }
}

fn call_host(cpu: &dyn Cpu, function: &HostFunction) {
    let mut kinds = function.parameters.clone();
    let mut classes: Vec<Class> = kinds.iter().filter_map(|&kind| Class::of(kind)).collect();
    let fixed = classes.len();
    let mut values: Vec<u64> = guest_layout(&classes)
        .slots
        .iter()
        .map(|&slot| read_slot(cpu, slot))
        .collect();
    match function.variadic {
        Variadic::No => {}
        Variadic::Format | Variadic::Scan => {
            let Some(&format) = values.last() else {
                fail(cpu, format_args!("{} has no format", function.name));
            };
            let scan = function.variadic == Variadic::Scan;
            let extra = format_classes(cpu, &function.name, format, scan);
            classes.extend(&extra);
            let layout = guest_layout(&classes);
            for (index, &class) in extra.iter().enumerate() {
                values.push(read_slot(cpu, layout.slots[fixed + index]));
                kinds.push(variadic_kind(class));
            }
        }
        Variadic::FormatVaList | Variadic::ScanVaList => {
            let Some(&format) = values.last() else {
                fail(cpu, format_args!("{} has no format", function.name));
            };
            let mut with_list = classes.clone();
            with_list.push(Class::Integer);
            let list = read_slot(cpu, guest_layout(&with_list).slots[fixed]);
            let scan = function.variadic == Variadic::ScanVaList;
            let extra = format_classes(cpu, &function.name, format, scan);
            values.extend(va_arguments(cpu, &function.name, list, &extra));
            kinds.extend(extra.iter().map(|&class| variadic_kind(class)));
            classes.extend(extra);
        }
        Variadic::Integers => {
            let used = classes.iter().filter(|&&c| c == Class::Integer).count();
            for _ in used..abi::GUEST_INTEGER_REGISTERS {
                classes.push(Class::Integer);
            }
            let layout = guest_layout(&classes);
            for &slot in &layout.slots[fixed..] {
                values.push(read_slot(cpu, slot));
                kinds.push(Type::U64);
            }
        }
    }

    let host = host_layout(&classes);
    let mut frame = native::HostFrame::default();
    let mut stack = vec![0u64; host.stack_slots];
    for (index, (&raw, &to)) in values.iter().zip(&host.slots).enumerate() {
        let mut value = normalize(kinds[index], raw);
        if let Some(&(_, signature)) = function.callbacks.iter().find(|(at, _)| *at == index)
            && value != 0
            && crate::loader::is_guest_address(value as usize)
        {
            value = match callback(value, signature, Kind::Plain) {
                Ok(thunk) => thunk as u64,
                Err(reason) => fail(cpu, format_args!("{}: {reason}", function.name)),
            };
        }
        if index == 0 {
            value = match function.target {
                Target::Fixed(_) if function.takes_env => jni::host_env(value),
                Target::Fixed(_) => value,
                Target::JniEnv(_) => jni::host_env(value),
                Target::JavaVm(_) => jni::host_vm(value),
            };
        }
        match to {
            Slot::Integer(register) => frame.gpr[register] = value,
            Slot::Vector(register) => frame.xmm[register] = value,
            Slot::Stack(slot) => stack[slot] = value,
        }
    }
    frame.function = match function.target {
        Target::Fixed(address) => address as u64,
        Target::JniEnv(slot) => jni::env_function(frame.gpr[0], slot),
        Target::JavaVm(slot) => jni::vm_function(frame.gpr[0], slot),
    };
    if frame.function == 0 {
        fail(
            cpu,
            format_args!("{} is null in the host table", function.name),
        );
    }
    // SAFETY: the address is a host function with this signature, from its table; the frame holds
    // exactly the arguments the signature declares, placed by the System V rules.
    unsafe { native::host_call(&mut frame, &stack) };
    set_result(cpu, function.returns, frame.rax, frame.xmm0);
}

fn set_result(cpu: &dyn Cpu, returns: Type, integer: u64, vector: u64) {
    match Class::of(returns) {
        Some(Class::Integer) => cpu.set_x(0, normalize(returns, integer)),
        Some(Class::Vector) => cpu.set_v(0, [normalize(returns, vector), 0]),
        None => {}
    }
}

// --- Threads: guest state per host thread ---

/// bionic's TLS slots: `TPIDR_EL0` points at slot 0, with slot -1 below it, and the stack guard is
/// slot 5, which code built with a stack protector reads at offset 0x28.
const TLS_WORDS: usize = 16;
const TLS_FIRST_SLOT: usize = 1;
const TLS_SLOT_STACK_GUARD: usize = 5;
/// Where the value of `TPIDR_EL0` itself is kept, ahead of the slots.
const TLS_REGISTER: usize = 0;
const TLS_BLOCK_WORDS: usize = 1 + TLS_WORDS;
/// No red zone exists in AAPCS64, but a nested call leaves room below the outer frame anyway.
const NESTED_GAP: u64 = 256;
/// Below translated code a signal interrupted: its frame may already be written below the stack
/// pointer the engine last stored.
const INTERRUPTED_GAP: u64 = 64 << 10;

struct Thread {
    /// The register, then the slots it points at.
    tls: Box<[u64; TLS_BLOCK_WORDS]>,
    stack: native::Stack,
    cpus: Vec<Box<dyn Cpu>>,
    depth: usize,
}

thread_local! {
    static THREAD: Cell<*mut Thread> = const { Cell::new(std::ptr::null_mut()) };
}

fn current_thread() -> Result<*mut Thread, String> {
    let existing = THREAD.with(Cell::get);
    if !existing.is_null() {
        return Ok(existing);
    }
    let stack = native::Stack::new().ok_or("cannot map a guest stack")?;
    let mut tls = Box::new([0u64; TLS_BLOCK_WORDS]);
    let slots = TLS_REGISTER + 1;
    tls[slots + TLS_FIRST_SLOT + TLS_SLOT_STACK_GUARD] = native::random_u64();
    tls[TLS_REGISTER] = std::ptr::from_ref(&tls[slots + TLS_FIRST_SLOT]) as u64;
    let thread = Box::into_raw(Box::new(Thread {
        tls,
        stack,
        cpus: Vec::new(),
        depth: 0,
    }));
    THREAD.with(|cell| cell.set(thread));
    native::on_thread_exit(thread.cast(), drop_thread);
    Ok(thread)
}

/// Frees a thread's guest state when the thread exits. Destructors of guest code that run later
/// make a new one.
extern "C" fn drop_thread(thread: *mut std::ffi::c_void) {
    THREAD.with(|cell| {
        if cell.get() == thread.cast() {
            cell.set(std::ptr::null_mut());
        }
    });
    // SAFETY: `current_thread` registered this pointer from `Box::into_raw`, and only this
    // thread's exit gets here, once.
    drop(unsafe { Box::from_raw(thread.cast::<Thread>()) });
}

/// The CPU for a call at `depth`, made on first use.
///
/// # Safety
/// `thread` must be the calling thread's state, from `current_thread`.
unsafe fn cpu_at(thread: *mut Thread, depth: usize) -> Result<*const dyn Cpu, String> {
    // SAFETY: the caller's contract; only short borrows are taken, and no other borrow of this
    // thread's state is alive across them.
    unsafe {
        while (*thread).cpus.len() <= depth {
            let tpidr = (*thread).tls.as_mut_ptr().add(TLS_REGISTER);
            let cpu = engine()?.cpu(tpidr, supervisor)?;
            (*thread).cpus.push(cpu);
        }
        let cpus = &(*thread).cpus;
        Ok(&*cpus[depth] as *const dyn Cpu)
    }
}

/// Runs the guest function at `target` with `values`, one per parameter, and returns x0 and the
/// low half of v0.
pub(crate) fn call(target: u64, parameters: &[Type], values: &[u64]) -> Result<(u64, u64), String> {
    let return_to = return_address()?;
    let thread = current_thread()?;
    // SAFETY: `thread` is this thread's state; each access below is a short borrow, and the
    // nested calls a guest run makes see the depth already raised, so they use another CPU.
    unsafe {
        let depth = (*thread).depth;
        let cpu = &*cpu_at(thread, depth)?;
        let top = if depth == 0 {
            (*thread).stack.top()
        } else {
            let cpus = &(*thread).cpus;
            let gap = match SERVING.with(Cell::get) < depth {
                true => INTERRUPTED_GAP,
                false => NESTED_GAP,
            };
            cpus[depth - 1].sp().saturating_sub(gap)
        };

        let classes: Vec<Class> = parameters.iter().filter_map(|&k| Class::of(k)).collect();
        let layout = guest_layout(&classes);
        let stack_bytes = (layout.stack_slots as u64 * 8).next_multiple_of(16);
        let sp = (top & !15).saturating_sub(stack_bytes);
        if sp < (*thread).stack.bottom() {
            return Err("guest stack overflow".to_string());
        }
        for ((&slot, &value), &kind) in layout.slots.iter().zip(values).zip(parameters) {
            let value = normalize(kind, value);
            match slot {
                Slot::Integer(index) => cpu.set_x(index, value),
                Slot::Vector(index) => cpu.set_v(index, [value, 0]),
                Slot::Stack(index) => native::write_u64(sp + 8 * index as u64, value),
            }
        }
        cpu.set_x(30, return_to);
        cpu.set_sp(sp);
        cpu.set_pc(target);

        (*thread).depth = depth + 1;
        let result = cpu.run();
        (*thread).depth = depth;
        result.map_err(|reason| {
            format!("{reason} at {}", crate::loader::describe_address(cpu.pc()))
        })?;
        Ok((cpu.x(0), cpu.v(0)[0]))
    }
}

/// The low and high end of the calling thread's guest stack, which every guest run on the thread
/// uses; none before its first guest call.
pub(super) fn current_stack() -> Option<(u64, u64)> {
    let thread = THREAD.with(Cell::get);
    // SAFETY: this thread's own state, alive until the thread exits; only the stack is read.
    (!thread.is_null()).then(|| unsafe { ((*thread).stack.bottom(), (*thread).stack.top()) })
}

/// The calling thread's guest state, as an identity, and how many guest runs it is inside.
pub(super) fn guest_context() -> (u64, usize) {
    let thread = THREAD.with(Cell::get);
    if thread.is_null() {
        return (0, 0);
    }
    // SAFETY: this thread's own state; only the depth is read.
    (thread as u64, unsafe { (*thread).depth })
}

/// What arm64 code learns about its CPU: ARMv8.0 with floating point (bit 0) and Advanced SIMD
/// (bit 1), the features every engine implements, never the host's x86 feature bits.
pub(super) const GUEST_HWCAP: u64 = (1 << 0) | (1 << 1);

/// Runs a guest ifunc resolver as bionic calls one on arm64: `HWCAP | _IFUNC_ARG_HWCAP` and an
/// `__ifunc_arg_t` of its size, HWCAP and HWCAP2.
pub(crate) fn call_ifunc(resolver: u64) -> Result<u64, String> {
    const IFUNC_ARG_HWCAP: u64 = 1 << 62;
    static ARGUMENT: [u64; 3] = [24, GUEST_HWCAP, 0];
    call(
        resolver,
        &[Type::U64, Type::Ptr],
        &[GUEST_HWCAP | IFUNC_ARG_HWCAP, ARGUMENT.as_ptr() as u64],
    )
    .map(|(x0, _)| x0)
}

/// Runs a guest initializer, `void(int, char**, char**)`.
pub(crate) fn call_initializer(address: u64, environment: u64) -> Result<(), String> {
    call(
        address,
        &[Type::I32, Type::Ptr, Type::Ptr],
        &[0, 0, environment],
    )
    .map(|_| ())
}

/// Calls `int name(void)` in guest code.
pub(crate) fn call_i32(address: u64) -> Result<i32, String> {
    call(address, &[], &[]).map(|(x0, _)| x0 as i32)
}

// --- Callbacks: host calls into guest code ---

/// What a callback does to its first argument before the guest sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Plain,
    /// A JNI method: the host `JNIEnv` becomes the guest's.
    Jni,
    /// `JNI_OnLoad`: the host `JavaVM` becomes the guest's.
    OnLoad,
    /// A signal handler: the third argument, the host's x86-64 `ucontext_t`, becomes null.
    Signal,
}

pub(crate) struct Callback {
    target: u64,
    returns: Type,
    parameters: Vec<Type>,
    kind: Kind,
}

/// A host function pointer that runs the guest function at `target`, which has `signature`.
pub(crate) fn callback(target: u64, signature: &str, kind: Kind) -> Result<usize, String> {
    type Key = (u64, String, Kind);
    static CALLBACKS: Mutex<Option<HashMap<Key, usize>>> = Mutex::new(None);
    let mut callbacks = CALLBACKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let callbacks = callbacks.get_or_insert_with(HashMap::new);
    let key = (target, signature.to_string(), kind);
    if let Some(&thunk) = callbacks.get(&key) {
        return Ok(thunk);
    }
    let Ok(Signature::Function {
        returns,
        parameters,
        variadic: false,
    }) = Signature::parse(signature)
    else {
        return Err(format!(
            "callback signature {signature:?} cannot be carried"
        ));
    };
    if parameters.contains(&Type::VaList) {
        return Err(format!("callback signature {signature:?} has a va_list"));
    }
    let context = Box::leak(Box::new(Callback {
        target,
        returns,
        parameters,
        kind,
    }));
    let thunk = native::thunk(std::ptr::from_ref(context)).ok_or("cannot map host thunks")?;
    callbacks.insert(key, thunk);
    Ok(thunk)
}

/// A host function pointer that calls the guest function at `address`, which has `signature`,
/// for host code that calls guest code directly, such as tests and benchmarks.
pub fn host_function(address: u64, signature: &str) -> Result<usize, String> {
    callback(address, signature, Kind::Plain)
}

/// Runs a callback for its host thunk: the frame holds the System V argument registers, and
/// gets the result.
fn enter(callback: &Callback, frame: &mut native::EntryFrame) {
    let classes: Vec<Class> = callback
        .parameters
        .iter()
        .filter_map(|&k| Class::of(k))
        .collect();
    let host = host_layout(&classes);
    let mut values = Vec::with_capacity(classes.len());
    for (index, &slot) in host.slots.iter().enumerate() {
        let raw = match slot {
            Slot::Integer(register) => frame.gpr[register],
            Slot::Vector(register) => frame.xmm[register],
            // SAFETY: the caller's stack arguments start where the thunk recorded.
            Slot::Stack(slot) => unsafe { native::read_u64(frame.stack + 8 * slot as u64) },
        };
        let mut value = normalize(callback.parameters[index], raw);
        value = match (callback.kind, index) {
            (Kind::Jni, 0) => jni::guest_env(value),
            (Kind::OnLoad, 0) => jni::guest_vm(value),
            (Kind::Signal, 2) => 0,
            _ => value,
        };
        values.push(value);
    }
    match call(callback.target, &callback.parameters, &values) {
        Ok((integer, vector)) => {
            frame.rax = normalize(callback.returns, integer);
            frame.xmm0 = normalize(callback.returns, vector);
        }
        Err(reason) => crate::shims::die(format_args!("uxdroid: guest code stopped: {reason}\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct NoCpus;

    impl Engine for NoCpus {
        fn name(&self) -> &str {
            "test"
        }
        unsafe fn cpu(&self, _: *mut u64, _: Supervisor) -> Result<Box<dyn Cpu>, String> {
            Err("no CPUs".to_string())
        }
    }

    static AVAILABLE: AtomicBool = AtomicBool::new(false);

    fn flaky() -> Result<Box<dyn Engine>, String> {
        match AVAILABLE.load(Ordering::SeqCst) {
            true => Ok(Box::new(NoCpus)),
            false => Err("cannot open libuxdroid_dynarmic.so".to_string()),
        }
    }

    #[test]
    fn unsigned_bionic_lists_what_no_table_or_hand_written_call_covers() {
        let unsigned = unsigned_bionic();
        let reason = |name: &str| {
            unsigned
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, why)| *why)
        };
        assert_eq!(
            reason("epoll_pwait2"),
            Some("struct epoll_event differs on arm64")
        );
        // Written by hand or carried by a twin, so no longer unsigned.
        for covered in [
            "qsort",
            "pthread_create",
            "vsnprintf",
            "epoll_wait",
            "stat",
            "socket",
        ] {
            assert_eq!(reason(covered), None, "{covered}");
        }
    }

    #[test]
    fn services_never_take_the_system_call_number() {
        assert_eq!(SYSCALL_SERVICE, 0);
        assert_eq!(RETURN_SERVICE, 0xFFFF);
        assert!(NEXT_SERVICE.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn a_failed_engine_load_is_tried_again() {
        static SLOT: EngineSlot = EngineSlot::new();
        assert_eq!(
            SLOT.get(None).err().as_deref(),
            Some("no translator is installed in this process")
        );
        assert_eq!(
            SLOT.get(Some(flaky)).err().as_deref(),
            Some("cannot open libuxdroid_dynarmic.so")
        );
        AVAILABLE.store(true, Ordering::SeqCst);
        assert_eq!(
            SLOT.get(Some(flaky)).map(|engine| engine.name()),
            Ok("test")
        );
        AVAILABLE.store(false, Ordering::SeqCst);
        assert_eq!(
            SLOT.get(Some(flaky)).map(|engine| engine.name()),
            Ok("test")
        );
    }
}
