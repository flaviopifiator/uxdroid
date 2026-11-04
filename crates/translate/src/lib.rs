//! The `arm64-v8a` engine of the native bridge: Dynarmic's A64 JIT behind the bridge's
//! [`Engine`] trait.
//!
//! Dynarmic is C++, built in Docker into `libuxdroid_dynarmic.so` (`docker/translation`), and
//! opened with `dlopen` the first time a guest library loads, so a process with host libraries only
//! never maps it. `UXDROID_TRANSLATOR` names the file; without it, the library is looked for next
//! to the one this crate is linked into.
//!
//! Every guest CPU of the process runs code from one shared code cache, so a block is translated
//! once, not again on each thread; the locking rules are at the top of `shim/dynarmic_shim.cpp`.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::cell::RefCell;
use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use uxdroid_bridge::guest::{Cpu, Engine, Supervisor, set_engine_loader};

const ABI_VERSION: u32 = 3;
const LIBRARY: &str = "libuxdroid_dynarmic.so";
/// Dynarmic's `HaltReason` bits that `uxd_cpu_run` returns.
const HALT_CACHE_INVALIDATION: u32 = 0x0000_0002;
/// Set by `HaltExecution()`, which the shim calls when a service ends the run or code faults.
const HALT_USER_DEFINED_1: u32 = 0x0100_0000;

/// Makes the bridge load this engine for the first guest library.
pub fn install() {
    set_engine_loader(load);
}

/// What the engine has translated, process-wide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Stats {
    /// Blocks translated, in every code cache.
    pub blocks: u64,
    /// Bytes of translated code in the current code cache.
    pub code_bytes: u64,
    /// Code caches made: one, plus one each time a cache was full or retired.
    pub caches: u64,
    /// Times the code at the address given to [`watch_translations`] was read to translate it.
    pub watched_translations: u64,
}

/// The loaded engine's functions; none before the first guest library loads.
static LOADED: OnceLock<&'static Api> = OnceLock::new();

/// The engine's counters, once it is loaded.
pub fn stats() -> Option<Stats> {
    let api = LOADED.get()?;
    let mut stats = Stats::default();
    // SAFETY: the shim fills the struct it is given, which has its layout.
    unsafe { (api.stats)(&mut stats) };
    Some(stats)
}

/// Counts, from now on, each time the guest code at `address` is translated, for
/// [`Stats::watched_translations`]. One address is watched at a time.
pub fn watch_translations(address: u64) -> bool {
    let Some(api) = LOADED.get() else {
        return false;
    };
    // SAFETY: a plain value; the shim only compares addresses with it.
    unsafe { (api.watch)(address) };
    true
}

/// Makes the current code cache take no more code, as when it is full, and starts a new one: each
/// thread moves to it when it next enters guest code, or at the first block the old cache lacks,
/// and the old one is freed when its last thread has left it.
pub fn retire_code_cache() -> bool {
    let Some(api) = LOADED.get() else {
        return false;
    };
    // SAFETY: no arguments; the shim takes its own lock.
    unsafe { (api.retire_cache)() };
    true
}

#[repr(C)]
struct Callbacks {
    context: *mut c_void,
    svc: extern "C" fn(*mut c_void, u32) -> c_int,
    exception: extern "C" fn(*mut c_void, u64, u32),
}

#[repr(C)]
struct RawCpu {
    _private: [u8; 0],
}

/// The library's functions, as `dynarmic_shim.cpp` declares them.
struct Api {
    cpu_new: unsafe extern "C" fn(*const Callbacks, *mut u64, *mut c_char, usize) -> *mut RawCpu,
    cpu_free: unsafe extern "C" fn(*mut RawCpu),
    run: unsafe extern "C" fn(*mut RawCpu) -> u32,
    get_x: unsafe extern "C" fn(*const RawCpu, u32) -> u64,
    set_x: unsafe extern "C" fn(*mut RawCpu, u32, u64),
    get_sp: unsafe extern "C" fn(*const RawCpu) -> u64,
    set_sp: unsafe extern "C" fn(*mut RawCpu, u64),
    get_pc: unsafe extern "C" fn(*const RawCpu) -> u64,
    set_pc: unsafe extern "C" fn(*mut RawCpu, u64),
    get_v: unsafe extern "C" fn(*const RawCpu, u32, *mut u64),
    set_v: unsafe extern "C" fn(*mut RawCpu, u32, *const u64),
    stats: unsafe extern "C" fn(*mut Stats),
    watch: unsafe extern "C" fn(u64),
    retire_cache: unsafe extern "C" fn(),
    translated: unsafe extern "C" fn(*mut u64, usize) -> usize,
    pretranslate: unsafe extern "C" fn(*const u64, usize) -> usize,
}

fn library_path() -> PathBuf {
    if let Some(path) = std::env::var_os("UXDROID_TRANSLATOR") {
        return PathBuf::from(path);
    }
    // SAFETY: zero is valid for this plain C struct, and dladdr only fills it in.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: the address is a function of this crate, which the process has mapped.
    let found = unsafe { libc::dladdr(install as *const c_void, &mut info) } != 0;
    if found && !info.dli_fname.is_null() {
        // SAFETY: dladdr returns a C string owned by the dynamic linker.
        let own = unsafe { CStr::from_ptr(info.dli_fname) };
        if let Some(dir) = std::path::Path::new(OsStr::from_bytes(own.to_bytes())).parent() {
            return dir.join(LIBRARY);
        }
    }
    PathBuf::from(LIBRARY)
}

fn load() -> Result<Box<dyn Engine>, String> {
    let path = library_path();
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "bad translator path")?;
    // SAFETY: the name is a C string. The library runs no code of ours at load but its own
    // static constructors, which are Dynarmic's and its dependencies'.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a C string or null.
        let reason = unsafe { libc::dlerror() };
        let reason = if reason.is_null() {
            "unknown error".into()
        } else {
            // SAFETY: as above.
            unsafe { CStr::from_ptr(reason) }.to_string_lossy()
        };
        return Err(format!("cannot open {}: {reason}", path.display()));
    }
    let symbol = |name: &CStr| -> Result<*mut c_void, String> {
        // SAFETY: the handle is open and the name a C string.
        let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if address.is_null() {
            Err(format!("lacks {}", name.to_string_lossy()))
        } else {
            Ok(address)
        }
    };
    // SAFETY: each symbol is the shim's function of the declared signature; the version check
    // below refuses a library made for another interface.
    let api = unsafe { api(&symbol) };
    match api {
        Ok(api) => {
            let api: &'static Api = Box::leak(Box::new(api));
            let _ = LOADED.set(api);
            Ok(Box::new(Dynarmic {
                api,
                path,
                identity: OnceLock::new(),
            }))
        }
        Err(reason) => {
            // SAFETY: the handle is open, and nothing of the library is in use.
            unsafe { libc::dlclose(handle) };
            Err(format!("{}: {reason}", path.display()))
        }
    }
}

/// The shim's functions, found with `symbol`.
///
/// # Safety
/// `symbol` must return addresses of the shim's functions by those names.
unsafe fn api(symbol: &dyn Fn(&CStr) -> Result<*mut c_void, String>) -> Result<Api, String> {
    // SAFETY: the caller's contract.
    unsafe {
        let version: unsafe extern "C" fn() -> u32 = cast(symbol(c"uxd_abi_version")?);
        let version = version();
        if version != ABI_VERSION {
            return Err(format!("interface {version}, not {ABI_VERSION}"));
        }
        Ok(Api {
            cpu_new: cast(symbol(c"uxd_cpu_new")?),
            cpu_free: cast(symbol(c"uxd_cpu_free")?),
            run: cast(symbol(c"uxd_cpu_run")?),
            get_x: cast(symbol(c"uxd_get_x")?),
            set_x: cast(symbol(c"uxd_set_x")?),
            get_sp: cast(symbol(c"uxd_get_sp")?),
            set_sp: cast(symbol(c"uxd_set_sp")?),
            get_pc: cast(symbol(c"uxd_get_pc")?),
            set_pc: cast(symbol(c"uxd_set_pc")?),
            get_v: cast(symbol(c"uxd_get_v")?),
            set_v: cast(symbol(c"uxd_set_v")?),
            stats: cast(symbol(c"uxd_stats_get")?),
            watch: cast(symbol(c"uxd_watch")?),
            retire_cache: cast(symbol(c"uxd_retire_cache")?),
            translated: cast(symbol(c"uxd_translated")?),
            pretranslate: cast(symbol(c"uxd_pretranslate")?),
        })
    }
}

/// A function pointer of type `F` from a symbol's address.
///
/// # Safety
/// The address must be a function of type `F`.
unsafe fn cast<F: Copy>(address: *mut c_void) -> F {
    assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
    // SAFETY: the caller's contract; the sizes match.
    unsafe { std::mem::transmute_copy(&address) }
}

struct Dynarmic {
    api: &'static Api,
    /// The engine's file, whose contents are its identity.
    path: PathBuf,
    identity: OnceLock<u64>,
}

impl Engine for Dynarmic {
    fn name(&self) -> &str {
        "dynarmic"
    }

    unsafe fn cpu(
        &self,
        tpidr_el0: *mut u64,
        supervisor: Supervisor,
    ) -> Result<Box<dyn Cpu>, String> {
        let mut cpu = Box::new(DynarmicCpu {
            api: self.api,
            raw: std::ptr::null_mut(),
            supervisor,
            fault: RefCell::new(None),
            callbacks: Callbacks {
                context: std::ptr::null_mut(),
                svc,
                exception,
            },
        });
        // The box never moves, so the shim may keep both pointers.
        cpu.callbacks.context = std::ptr::from_ref::<DynarmicCpu>(&cpu).cast_mut().cast();
        let mut error = [0 as c_char; 256];
        // SAFETY: the callbacks live in the box with the CPU; the caller keeps `tpidr_el0` valid
        // for the CPU's life; the error buffer is writable for its length, less one byte, so it
        // stays terminated whatever the shim writes.
        let raw = unsafe {
            (self.api.cpu_new)(
                &cpu.callbacks,
                tpidr_el0,
                error.as_mut_ptr(),
                error.len() - 1,
            )
        };
        if raw.is_null() {
            let bytes: Vec<u8> = error
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|&byte| byte as u8)
                .collect();
            return Err(format!("dynarmic: {}", String::from_utf8_lossy(&bytes)));
        }
        cpu.raw = raw;
        Ok(cpu)
    }

    fn translated(&self) -> Vec<u64> {
        let mut locations = Vec::new();
        loop {
            // SAFETY: the shim writes at most `capacity` words into the buffer.
            let count =
                unsafe { (self.api.translated)(locations.as_mut_ptr(), locations.capacity()) };
            if count <= locations.capacity() {
                // SAFETY: the shim wrote `count` words, within the capacity.
                unsafe { locations.set_len(count) };
                return locations;
            }
            // More than room for: blocks were added meanwhile; ask again with room to spare.
            locations.reserve(count + count / 8);
        }
    }

    unsafe fn pretranslate(&self, locations: &[u64]) -> usize {
        // SAFETY: the slice is valid for its length; the caller keeps the code readable.
        unsafe { (self.api.pretranslate)(locations.as_ptr(), locations.len()) }
    }

    fn identity(&self) -> u64 {
        // FNV-1a of the engine's file and its interface version, computed once, when first asked,
        // which is off the thread starting the app.
        *self.identity.get_or_init(|| {
            let bytes = std::fs::read(&self.path).unwrap_or_default();
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for &byte in ABI_VERSION.to_le_bytes().iter().chain(&bytes) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash
        })
    }
}

struct DynarmicCpu {
    api: &'static Api,
    raw: *mut RawCpu,
    supervisor: Supervisor,
    fault: RefCell<Option<String>>,
    callbacks: Callbacks,
}

extern "C" fn svc(context: *mut c_void, number: u32) -> c_int {
    let halt = std::panic::catch_unwind(|| {
        // SAFETY: the context is the CPU the shim was made with, alive while it runs.
        let cpu = unsafe { &*context.cast::<DynarmicCpu>() };
        (cpu.supervisor)(cpu, number)
    });
    match halt {
        Ok(halt) => c_int::from(halt),
        // Unwinding into C++ frames is undefined.
        Err(_) => std::process::abort(),
    }
}

extern "C" fn exception(context: *mut c_void, pc: u64, kind: u32) {
    let reason = match kind {
        1 => "an unallocated encoding",
        2 => "a reserved value",
        3 => "an unpredictable instruction",
        4 => "a breakpoint",
        5 => "a jump to memory that is not executable",
        7 => "an instruction the JIT does not implement",
        _ => "an exception",
    };
    // SAFETY: as for `svc`.
    let cpu = unsafe { &*context.cast::<DynarmicCpu>() };
    if let Ok(mut fault) = cpu.fault.try_borrow_mut() {
        *fault = Some(format!("{reason} at pc {pc:#x}"));
    }
}

// SAFETY for every method below: `raw` is the live CPU this struct owns, used only by its thread,
// and indices are checked against the register file's size.
impl Cpu for DynarmicCpu {
    fn x(&self, index: usize) -> u64 {
        assert!(index < 31);
        unsafe { (self.api.get_x)(self.raw, index as u32) }
    }
    fn set_x(&self, index: usize, value: u64) {
        assert!(index < 31);
        unsafe { (self.api.set_x)(self.raw, index as u32, value) }
    }
    fn sp(&self) -> u64 {
        unsafe { (self.api.get_sp)(self.raw) }
    }
    fn set_sp(&self, value: u64) {
        unsafe { (self.api.set_sp)(self.raw, value) }
    }
    fn pc(&self) -> u64 {
        unsafe { (self.api.get_pc)(self.raw) }
    }
    fn set_pc(&self, value: u64) {
        unsafe { (self.api.set_pc)(self.raw, value) }
    }
    fn v(&self, index: usize) -> [u64; 2] {
        assert!(index < 32);
        let mut value = [0u64; 2];
        unsafe { (self.api.get_v)(self.raw, index as u32, value.as_mut_ptr()) };
        value
    }
    fn set_v(&self, index: usize, value: [u64; 2]) {
        assert!(index < 32);
        unsafe { (self.api.set_v)(self.raw, index as u32, value.as_ptr()) }
    }
    fn run(&self) -> Result<(), String> {
        loop {
            let reason = unsafe { (self.api.run)(self.raw) };
            if let Some(fault) = self.fault.borrow_mut().take() {
                return Err(fault);
            }
            if reason & HALT_USER_DEFINED_1 != 0 {
                return Ok(());
            }
            // A run broken off to invalidate code goes on where it was. The shim never asks for
            // one now (its caches are never invalidated), but the reason stays harmless.
            if reason == HALT_CACHE_INVALIDATION {
                continue;
            }
            return Err(format!("the engine stopped with halt reason {reason:#x}"));
        }
    }
}

impl Drop for DynarmicCpu {
    fn drop(&mut self) {
        // A CPU the shim failed to make has nothing to free.
        if self.raw.is_null() {
            return;
        }
        // SAFETY: the CPU this struct made, no longer running.
        unsafe { (self.api.cpu_free)(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static FREED: AtomicU32 = AtomicU32::new(0);
    static RUNS: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn refuse(
        _: *const Callbacks,
        _: *mut u64,
        error: *mut c_char,
        len: usize,
    ) -> *mut RawCpu {
        let reason = b"too many guest CPUs\0";
        // SAFETY: the caller passes `len` writable bytes; the test's reason fits.
        unsafe {
            std::ptr::copy_nonoverlapping(reason.as_ptr().cast(), error, reason.len().min(len))
        };
        std::ptr::null_mut()
    }
    unsafe extern "C" fn make(
        _: *const Callbacks,
        _: *mut u64,
        _: *mut c_char,
        _: usize,
    ) -> *mut RawCpu {
        std::ptr::dangling_mut()
    }
    unsafe extern "C" fn free(_: *mut RawCpu) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }
    /// Invalidated once, then halted by a service.
    unsafe extern "C" fn run(_: *mut RawCpu) -> u32 {
        match RUNS.fetch_add(1, Ordering::SeqCst) {
            0 => HALT_CACHE_INVALIDATION,
            _ => HALT_USER_DEFINED_1,
        }
    }
    unsafe extern "C" fn get(_: *const RawCpu) -> u64 {
        0
    }
    unsafe extern "C" fn get_x(_: *const RawCpu, _: u32) -> u64 {
        0
    }
    unsafe extern "C" fn set(_: *mut RawCpu, _: u64) {}
    unsafe extern "C" fn set_x(_: *mut RawCpu, _: u32, _: u64) {}
    unsafe extern "C" fn get_v(_: *const RawCpu, _: u32, _: *mut u64) {}
    unsafe extern "C" fn set_v(_: *mut RawCpu, _: u32, _: *const u64) {}
    unsafe extern "C" fn stats(_: *mut Stats) {}
    unsafe extern "C" fn watch(_: u64) {}
    unsafe extern "C" fn retire_cache() {}
    unsafe extern "C" fn translated(_: *mut u64, _: usize) -> usize {
        0
    }
    unsafe extern "C" fn pretranslate(_: *const u64, _: usize) -> usize {
        0
    }
    fn supervisor(_: &dyn Cpu, _: u32) -> bool {
        true
    }

    fn engine(
        cpu_new: unsafe extern "C" fn(
            *const Callbacks,
            *mut u64,
            *mut c_char,
            usize,
        ) -> *mut RawCpu,
    ) -> Dynarmic {
        Dynarmic {
            api: Box::leak(Box::new(Api {
                cpu_new,
                cpu_free: free,
                run,
                get_x,
                set_x,
                get_sp: get,
                set_sp: set,
                get_pc: get,
                set_pc: set,
                get_v,
                set_v,
                stats,
                watch,
                retire_cache,
                translated,
                pretranslate,
            })),
            path: PathBuf::new(),
            identity: OnceLock::new(),
        }
    }

    #[test]
    fn a_cpu_the_shim_refused_is_never_freed() {
        let mut tpidr = 0u64;
        // SAFETY: `tpidr` outlives the attempt.
        let made = unsafe { engine(refuse).cpu(&mut tpidr, supervisor) };
        assert_eq!(made.err().as_deref(), Some("dynarmic: too many guest CPUs"));
        assert_eq!(FREED.load(Ordering::SeqCst), 0);

        // SAFETY: as above.
        let cpu = unsafe { engine(make).cpu(&mut tpidr, supervisor) }.unwrap();
        // A run the engine broke off to invalidate code goes on until a service halts it.
        assert_eq!(cpu.run(), Ok(()));
        assert_eq!(RUNS.load(Ordering::SeqCst), 2);
        drop(cpu);
        assert_eq!(FREED.load(Ordering::SeqCst), 1);
    }
}
