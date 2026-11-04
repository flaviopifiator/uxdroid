//! The `arm64-v8a` engine of the native bridge: Dynarmic's A64 JIT behind the bridge's
//! [`Engine`] trait.
//!
//! Dynarmic is C++, built in Docker into `libuxdroid_dynarmic.so` (`docker/translation`), and
//! opened with `dlopen` the first time a guest library loads, so a process with host libraries only
//! never maps it. `UXDROID_TRANSLATOR` names the file; without it, the library is looked for next
//! to the one this crate is linked into.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::cell::RefCell;
use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use uxdroid_bridge::guest::{Cpu, Engine, Supervisor, set_engine_loader};

const ABI_VERSION: u32 = 1;
const LIBRARY: &str = "libuxdroid_dynarmic.so";

/// Makes the bridge load this engine for the first guest library.
pub fn install() {
    set_engine_loader(load);
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
            Err(format!(
                "{} lacks {}",
                path.display(),
                name.to_string_lossy()
            ))
        } else {
            Ok(address)
        }
    };
    // SAFETY: each symbol is the shim's function of the declared signature; the version check
    // below refuses a library made for another interface.
    unsafe {
        let version: unsafe extern "C" fn() -> u32 = cast(symbol(c"uxd_abi_version")?);
        if version() != ABI_VERSION {
            return Err(format!(
                "{} has interface {}, not {ABI_VERSION}",
                path.display(),
                version()
            ));
        }
        let api = Api {
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
        };
        Ok(Box::new(Dynarmic {
            api: Box::leak(Box::new(api)),
        }))
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
        // for the CPU's life; the error buffer is writable for its length.
        let raw = unsafe { (self.api.cpu_new)(&cpu.callbacks, tpidr_el0, error.as_mut_ptr(), 256) };
        if raw.is_null() {
            // SAFETY: the shim writes a terminated string on failure.
            let reason = unsafe { CStr::from_ptr(error.as_ptr()) };
            return Err(format!("dynarmic: {}", reason.to_string_lossy()));
        }
        cpu.raw = raw;
        Ok(cpu)
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
        unsafe { (self.api.run)(self.raw) };
        match self.fault.borrow_mut().take() {
            Some(fault) => Err(fault),
            None => Ok(()),
        }
    }
}

impl Drop for DynarmicCpu {
    fn drop(&mut self) {
        // SAFETY: the CPU this struct made, no longer running.
        unsafe { (self.api.cpu_free)(self.raw) };
    }
}
