//! Bionic symbols that glibc lacks or defines with a different ABI.
//!
//! Struct sizes and constants below were measured by compiling one probe against both libcs
//! (`abi/compare.sh`): on x86_64 `sigset_t` is 8 bytes in bionic and 128 in glibc, `jmp_buf` 88
//! and 200, `fpos_t` 8 and 16; on aarch64 `jmp_buf` is 256 and 312, and `pthread_attr_t` and
//! `pthread_mutex_t` are larger in glibc too. `long double` has the same size with another format
//! on x86_64, binary128 and x87 80-bit; on aarch64 both are binary128.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_int, c_long, c_void};
use std::sync::{Mutex, OnceLock};

use crate::loader;
use crate::table::Table;

mod bionic;
pub(crate) mod data;

/// The Android API level uxdroid reports to libraries.
const API_LEVEL: &CStr = c"35";

enum Target {
    Function(*const ()),
    /// Another name for the same operation in glibc.
    Alias(&'static CStr),
}

// SAFETY: the pointers are addresses of immutable functions.
unsafe impl Send for Target {}
// SAFETY: as above.
unsafe impl Sync for Target {}

/// The address of `name` outside loaded libraries: a shim, or glibc if the symbol is audited.
///
/// A function glibc has but nobody audited gets a trap that names it when called, because it may
/// corrupt memory silently. A data symbol cannot trap, so it counts as not found.
pub fn resolve(name: &CStr, is_data: bool) -> Option<usize> {
    match decide(name, is_data) {
        Decision::Bound(address, _) => Some(address),
        Decision::Unaudited(text) => unaudited_trap(text),
        Decision::Missing(_) => None,
    }
}

/// How the bridge binds an import of a host library, for audits such as the coverage example.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBinding {
    /// A function or object of a signature table, by its library.
    Table(&'static str),
    /// Code uxdroid writes for bionic's ABI, or glibc's operation under its glibc name.
    Shim,
    /// glibc's own symbol, audited as compatible.
    Glibc,
    /// A stub that stops the program naming the function when called, with why.
    Trap(&'static str),
    /// A function of this NDK library that uxdroid does not have yet: a stub that stops the
    /// program naming the library and the function when called.
    Unimplemented(&'static str),
    /// Nothing binds: the load fails, or a weak import is null; with why.
    Missing(&'static str),
}

const NOT_AUDITED: &str = "glibc has it, not audited for bionic";

/// How a host library whose scope has `tables` and needs the NDK libraries `needed` binds a
/// non-weak import of `name`, decided as the loader does after the libraries in scope, and without
/// making a trap. liblog is always in scope, as on Android.
pub fn host_binding(
    name: &str,
    tables: &[&'static Table],
    needed: &[&str],
    is_data: bool,
) -> HostBinding {
    match bound_by_host(name, tables, is_data) {
        HostBinding::Missing(why) => {
            match crate::unimplemented::find(needed.iter().copied(), name, is_data, false) {
                Some(function) => HostBinding::Unimplemented(function.library),
                None => HostBinding::Missing(why),
            }
        }
        binding => binding,
    }
}

fn bound_by_host(name: &str, tables: &[&'static Table], is_data: bool) -> HostBinding {
    let liblog = crate::table::liblog();
    let in_scope = tables
        .iter()
        .copied()
        .chain((!tables.iter().any(|table| std::ptr::eq(*table, liblog))).then_some(liblog));
    for table in in_scope {
        if let Some(export) = table.find(name.as_bytes()) {
            return match export.bind() {
                Some(_) => HostBinding::Table(table.library),
                None => HostBinding::Missing("its table's host library lacks it"),
            };
        }
    }
    let Ok(name) = std::ffi::CString::new(name) else {
        return HostBinding::Missing("not a C string");
    };
    match decide(&name, is_data) {
        Decision::Bound(_, binding) => binding,
        Decision::Unaudited(_) => HostBinding::Trap(NOT_AUDITED),
        Decision::Missing(why) => HostBinding::Missing(why),
    }
}

enum Decision<'a> {
    Bound(usize, HostBinding),
    /// glibc has the function, but it binds to a trap naming it.
    Unaudited(&'a str),
    Missing(&'static str),
}

/// What `resolve` binds `name` to, before any trap is made.
fn decide(name: &CStr, is_data: bool) -> Decision<'_> {
    let Ok(text) = name.to_str() else {
        return Decision::Missing("not UTF-8");
    };
    match table().get(text) {
        Some(Target::Function(address)) => {
            let trapped = TRAPS
                .iter()
                .any(|&(known, trap)| known == text && std::ptr::eq(trap, *address));
            let binding = match trapped {
                true => HostBinding::Trap("an incompatible ABI and no shim yet"),
                false => HostBinding::Shim,
            };
            Decision::Bound(*address as usize, binding)
        }
        Some(Target::Alias(target)) => match host_symbol(target) {
            Some(address) => Decision::Bound(address, HostBinding::Shim),
            None => Decision::Missing("glibc lacks the name it is an alias of"),
        },
        None => match host_symbol(name) {
            None => Decision::Missing("no shim, and glibc lacks it"),
            Some(address) if audited().contains(text) => {
                Decision::Bound(address, HostBinding::Glibc)
            }
            Some(_) if is_data => Decision::Missing("glibc data, not audited, cannot trap"),
            Some(_) => Decision::Unaudited(text),
        },
    }
}

/// Whether the host has a function `name`, a shim or glibc's, audited or not, decided without
/// taking one of the stubs for unaudited functions.
#[cfg(target_arch = "x86_64")]
pub(crate) fn has_function(name: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    matches!(
        decide(&name, false),
        Decision::Bound(..) | Decision::Unaudited(_)
    )
}

/// Whether `name` binds to a shim or an audited glibc function, rather than a stub that stops
/// the program naming it.
#[cfg(target_arch = "x86_64")]
pub(crate) fn is_audited(name: &str) -> bool {
    table().contains_key(name) || audited().contains(name)
}

fn audited() -> &'static HashSet<&'static str> {
    static AUDITED: OnceLock<HashSet<&'static str>> = OnceLock::new();
    AUDITED.get_or_init(|| {
        include_str!("glibc_compatible.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .flat_map(str::split_whitespace)
            .collect()
    })
}

const UNAUDITED_SLOTS: usize = 4096;

// One small stub per unaudited symbol, each pushing its slot number. Having them in the binary
// means nothing is written at run time, and pages stay free of write-and-execute mappings.
#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".globl uxdroid_unaudited_stubs",
    ".p2align 4",
    "uxdroid_unaudited_stubs:",
    ".set uxdroid_slot, 0",
    ".rept 4096",
    ".p2align 4",
    "pushq $uxdroid_slot",
    "jmp uxdroid_unaudited_common",
    ".set uxdroid_slot, uxdroid_slot + 1",
    ".endr",
    "uxdroid_unaudited_common:",
    "popq %rdi",
    "subq $8, %rsp",
    "call {report}",
    report = sym report_unaudited,
    options(att_syntax),
);

// The same on AArch64: each 16-byte stub moves its slot number into the first argument register.
#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".globl uxdroid_unaudited_stubs",
    ".p2align 4",
    "uxdroid_unaudited_stubs:",
    ".set uxdroid_slot, 0",
    ".rept 4096",
    ".p2align 4",
    "mov x0, #uxdroid_slot",
    "b uxdroid_unaudited_common",
    ".set uxdroid_slot, uxdroid_slot + 1",
    ".endr",
    "uxdroid_unaudited_common:",
    "b {report}",
    report = sym report_unaudited,
);

unsafe extern "C" {
    fn uxdroid_unaudited_stubs();
}

static UNAUDITED_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn unaudited_trap(name: &str) -> Option<usize> {
    const STUB_BYTES: usize = 16;
    let mut names = UNAUDITED_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let slot = match names.iter().position(|known| known == name) {
        Some(slot) => slot,
        None if names.len() < UNAUDITED_SLOTS => {
            names.push(name.to_string());
            names.len() - 1
        }
        None => return None,
    };
    Some(uxdroid_unaudited_stubs as *const () as usize + slot * STUB_BYTES)
}

extern "C" fn report_unaudited(slot: usize) -> ! {
    let names = UNAUDITED_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let name = names.get(slot).map_or("unknown symbol", String::as_str);
    die(format_args!(
        "uxdroid: {} is not audited for bionic compatibility yet\n",
        name.escape_debug()
    ))
}

/// bionic starts in UTF-8 and a glibc process in the C locale, where valid multibyte text fails.
pub fn use_bionic_default_locale() {
    static LOCALE: OnceLock<()> = OnceLock::new();
    LOCALE.get_or_init(|| {
        // SAFETY: the name is a valid C string. This runs before any library code, and glibc
        // always has this locale built in.
        unsafe {
            libc::setlocale(libc::LC_CTYPE, c"C.UTF-8".as_ptr());
        }
    });
}

fn host_symbol(name: &CStr) -> Option<usize> {
    static LIBM: OnceLock<()> = OnceLock::new();
    LIBM.get_or_init(|| {
        // SAFETY: the name is a valid C string. Failure only leaves math symbols unresolved,
        // which the loader reports.
        unsafe {
            libc::dlopen(c"libm.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        }
    });
    // SAFETY: the name is a valid C string, and RTLD_DEFAULT needs no handle.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!address.is_null()).then_some(address as usize)
}

pub(crate) fn write_stderr(message: std::fmt::Arguments<'_>) {
    crate::output::write_stderr(message);
}

pub(crate) fn die(message: std::fmt::Arguments<'_>) -> ! {
    write_stderr(message);
    // SAFETY: abort takes no arguments and never returns.
    unsafe { libc::abort() }
}

/// Fails loudly where glibc's symbol would silently corrupt memory or return garbage.
fn trap(name: &str) -> ! {
    die(format_args!(
        "uxdroid: {name} has an incompatible ABI and no shim yet\n"
    ))
}

fn fortify_failure(name: &str) -> ! {
    die(format_args!("uxdroid: {name}: prevented buffer overflow\n"))
}

macro_rules! traps {
    ($($name:ident)*) => {
        mod trapped {
            $(#[allow(non_snake_case)]
            pub extern "C" fn $name() -> ! {
                super::trap(stringify!($name))
            })*
        }
        const TRAPS: &[(&str, *const ())] = &[
            $((stringify!($name), trapped::$name as *const ()),)*
        ];
    };
}

traps! {
    // Different buffer size.
    fgetpos fsetpos
    // Android's long double is binary128 in xmm0; glibc's is x87.
    strtold strtold_l wcstold wcstold_l
    acosl acoshl asinl asinhl atanl atan2l atanhl cbrtl ceill copysignl cosl coshl erfl erfcl
    expl exp2l expm1l fabsl fdiml floorl fmal fmaxl fminl fmodl frexpl hypotl ilogbl ldexpl
    lgammal llrintl llroundl logl log10l log1pl log2l logbl lrintl lroundl modfl nanl nearbyintl
    nextafterl nexttoward nexttowardl powl remainderl remquol rintl roundl scalblnl scalbnl sinl
    sinhl sqrtl tanl tanhl tgammal truncl
}

fn table() -> &'static HashMap<&'static str, Target> {
    static TABLE: OnceLock<HashMap<&'static str, Target>> = OnceLock::new();
    TABLE.get_or_init(|| {
        data::init_stack_guard();
        let mut table = HashMap::new();
        let objects = data::objects();
        for &(name, address) in TRAPS
            .iter()
            .chain(STREAM_FUNCTIONS)
            .chain(crate::paths::PATH_FUNCTIONS)
            .chain(bionic::FUNCTIONS)
            .chain(&objects)
        {
            table.insert(name, Target::Function(address));
        }
        table.insert(
            "__sF",
            Target::Function(std::ptr::from_ref(&LEGACY_STREAMS).cast()),
        );
        // On LP64 bionic, off64_t is off_t, so the 64 names are the same functions.
        table.insert("fseeko64", Target::Function(stdio::fseeko as *const ()));
        table.insert("ftello64", Target::Function(stdio::ftello as *const ()));
        for &(name, target) in ALIASES {
            table.insert(name, Target::Alias(target));
        }
        for &(name, address) in FUNCTIONS {
            table.insert(name, Target::Function(address.0));
        }
        #[cfg(target_arch = "aarch64")]
        for &(name, address) in wide_objects::FUNCTIONS.iter().chain(misaligned::FUNCTIONS) {
            table.insert(name, Target::Function(address));
        }
        table
    })
}

const ALIASES: &[(&str, &CStr)] = &[
    ("__errno", c"__errno_location"),
    ("__get_h_errno", c"__h_errno_location"),
    ("__gnu_basename", c"basename"),
    ("__gnu_strerror_r", c"strerror_r"),
    ("strerror_r", c"__xpg_strerror_r"),
];

#[derive(Clone, Copy)]
struct Address(*const ());
// SAFETY: the pointers are addresses of immutable functions.
unsafe impl Sync for Address {}

macro_rules! functions {
    ($($name:literal => $function:path,)*) => {
        const FUNCTIONS: &[(&str, Address)] = &[
            $(($name, Address($function as *const ())),)*
        ];
    };
}

functions! {
    "__FD_CLR_chk" => fd_clr_chk,
    "__FD_ISSET_chk" => fd_isset_chk,
    "__FD_SET_chk" => fd_set_chk,
    "__assert" => assert_failed,
    "__assert2" => assert2_failed,
    "__pthread_cleanup_pop" => pthread_cleanup_pop,
    "__pthread_cleanup_push" => pthread_cleanup_push,
    "__fgets_chk" => fgets_chk,
    "__fread_chk" => fread_chk,
    "__fwrite_chk" => fwrite_chk,
    "__snprintf_chk" => uxdroid_snprintf_chk,
    "__sprintf_chk" => uxdroid_sprintf_chk,
    "__strchr_chk" => strchr_chk,
    "__strlcat_chk" => strlcat_chk,
    "__strlcpy_chk" => strlcpy_chk,
    "__strlen_chk" => strlen_chk,
    "__strncpy_chk2" => strncpy_chk2,
    "__strrchr_chk" => strrchr_chk,
    "__vsnprintf_chk" => vsnprintf_chk,
    "__vsprintf_chk" => vsprintf_chk,
    "_longjmp" => uxdroid_longjmp,
    "_setjmp" => uxdroid_underscore_setjmp,
    "__system_property_area_serial" => system_property_area_serial,
    "__system_property_find" => system_property_find,
    "__system_property_find_nth" => system_property_find_nth,
    "__system_property_foreach" => system_property_foreach,
    "__system_property_get" => system_property_get,
    "__system_property_read" => system_property_read,
    "__system_property_read_callback" => system_property_read_callback,
    "__system_property_serial" => system_property_serial,
    "__system_property_set" => system_property_set,
    "__system_property_wait" => system_property_wait,
    "__system_property_wait_any" => system_property_wait_any,
    "__pwrite64_chk" => pwrite_chk,
    "__pwrite_chk" => pwrite_chk,
    "__umask_chk" => umask_chk,
    "__write_chk" => write_chk,
    "__loader_android_dlopen_ext" => loader::linker::android_dlopen_ext,
    "__loader_dl_iterate_phdr" => loader::dl_iterate_phdr,
    "__loader_dladdr" => loader::dladdr,
    "__loader_dlclose" => loader::dlclose,
    "__loader_dlerror" => loader::dlerror,
    "__loader_dlopen" => loader::linker::dlopen,
    "__loader_dlsym" => loader::linker::dlsym,
    "__loader_dlvsym" => loader::linker::dlvsym,
    "android_dlopen_ext" => loader::android_dlopen_ext,
    "android_set_abort_message" => set_abort_message,
    "asprintf" => uxdroid_asprintf,
    "basename" => basename,
    "dirname" => dirname,
    "dl_iterate_phdr" => loader::dl_iterate_phdr,
    "dprintf" => uxdroid_dprintf,
    "dladdr" => loader::dladdr,
    "dlclose" => loader::dlclose,
    "dlerror" => loader::dlerror,
    "dlopen" => loader::dlopen,
    "dlsym" => loader::dlsym,
    "epoll_pwait" => epoll_pwait,
    "epoll_pwait64" => epoll_pwait,
    "fpathconf" => fpathconf,
    "fprintf" => uxdroid_fprintf,
    "freeaddrinfo" => freeaddrinfo,
    "fscanf" => uxdroid_fscanf,
    "funopen" => funopen,
    "funopen64" => funopen,
    "fwprintf" => uxdroid_fwprintf,
    "fwscanf" => uxdroid_fwscanf,
    "gai_strerror" => gai_strerror,
    "getnameinfo" => getnameinfo,
    "getprogname" => getprogname,
    "longjmp" => uxdroid_longjmp,
    "getaddrinfo" => getaddrinfo,
    "nl_langinfo" => nl_langinfo,
    "pathconf" => pathconf,
    "ppoll" => ppoll,
    "ppoll64" => ppoll,
    "printf" => uxdroid_printf,
    "pselect" => pselect,
    "pselect64" => pselect,
    "pthread_atfork" => pthread_atfork,
    "pthread_exit" => pthread_exit,
    "pthread_gettid_np" => pthread_gettid_np,
    "pthread_sigmask" => pthread_sigmask,
    "pthread_sigmask64" => pthread_sigmask,
    "setbuf" => setbuf,
    "setjmp" => uxdroid_setjmp,
    "sigaction" => sigaction,
    "sigaction64" => sigaction,
    "sigaddset" => sigaddset,
    "sigaddset64" => sigaddset,
    "sigdelset" => sigdelset,
    "sigdelset64" => sigdelset,
    "sigemptyset" => sigemptyset,
    "sigemptyset64" => sigemptyset,
    "sigfillset" => sigfillset,
    "sigfillset64" => sigfillset,
    "sigismember" => sigismember,
    "sigismember64" => sigismember,
    "siglongjmp" => uxdroid_longjmp,
    "signalfd" => signalfd,
    "signalfd64" => signalfd,
    "sigpending" => sigpending,
    "sigpending64" => sigpending,
    "sigprocmask" => sigprocmask,
    "sigprocmask64" => sigprocmask,
    "sigsetjmp" => uxdroid_sigsetjmp,
    "sigsuspend" => sigsuspend,
    "sigsuspend64" => sigsuspend,
    "sigtimedwait" => sigtimedwait,
    "sigtimedwait64" => sigtimedwait,
    "sigwait" => sigwait,
    "sigwait64" => sigwait,
    "sigwaitinfo" => sigwaitinfo,
    "sigwaitinfo64" => sigwaitinfo,
    "snprintf" => uxdroid_snprintf,
    "sprintf" => uxdroid_sprintf,
    "sscanf" => uxdroid_sscanf,
    "swprintf" => uxdroid_swprintf,
    "sysconf" => sysconf,
    "syslog" => uxdroid_syslog,
    "vasprintf" => vasprintf,
    "vfprintf" => vfprintf,
    "vfscanf" => vfscanf,
    "vfwprintf" => vfwprintf,
    "vfwscanf" => vfwscanf,
    "vdprintf" => vdprintf,
    "vprintf" => vprintf,
    "vsnprintf" => checked_vsnprintf,
    "vsprintf" => vsprintf,
    "vsscanf" => vsscanf,
    "vswprintf" => vswprintf,
    "vsyslog" => vsyslog,
}

// --- Formatted output entry points ---

unsafe extern "C" {
    pub(crate) fn vsnprintf(
        buffer: *mut c_char,
        size: usize,
        format: *const c_char,
        args: *mut c_void,
    ) -> c_int;
    fn uxdroid_fprintf();
    fn uxdroid_fscanf();
    fn uxdroid_fwprintf();
    fn uxdroid_fwscanf();
    fn uxdroid_printf();
    fn uxdroid_sprintf();
    fn uxdroid_snprintf();
    fn uxdroid_asprintf();
    fn uxdroid_dprintf();
    fn uxdroid_sscanf();
    fn uxdroid_swprintf();
    fn uxdroid_syslog();
    fn uxdroid_sprintf_chk();
    fn uxdroid_snprintf_chk();
    fn uxdroid_setjmp();
    fn uxdroid_underscore_setjmp();
    fn uxdroid_sigsetjmp();
    fn uxdroid_longjmp();
    fn __vsprintf_chk(
        buffer: *mut c_char,
        flags: c_int,
        size: usize,
        format: *const c_char,
        args: *mut c_void,
    ) -> c_int;
    fn __vsnprintf_chk(
        buffer: *mut c_char,
        size: usize,
        flags: c_int,
        object_size: usize,
        format: *const c_char,
        args: *mut c_void,
    ) -> c_int;
    #[link_name = "vsprintf"]
    fn host_vsprintf(buffer: *mut c_char, format: *const c_char, args: *mut c_void) -> c_int;
    #[link_name = "vprintf"]
    fn host_vprintf(format: *const c_char, args: *mut c_void) -> c_int;
    #[link_name = "vasprintf"]
    fn host_vasprintf(buffer: *mut *mut c_char, format: *const c_char, args: *mut c_void) -> c_int;
    #[link_name = "vdprintf"]
    fn host_vdprintf(fd: c_int, format: *const c_char, args: *mut c_void) -> c_int;
    #[link_name = "vsscanf"]
    fn host_vsscanf(text: *const c_char, format: *const c_char, args: *mut c_void) -> c_int;
    #[link_name = "vswprintf"]
    fn host_vswprintf(
        buffer: *mut i32,
        size: usize,
        format: *const i32,
        args: *mut c_void,
    ) -> c_int;
    #[link_name = "vsyslog"]
    fn host_vsyslog(priority: c_int, format: *const c_char, args: *mut c_void);
}

// Stable Rust cannot define C-variadic functions, so these entry points build the System V
// `va_list` by hand: 24 bytes of header at rsp, then the register save area, then the caller's
// stack arguments past our frame. `$named_bytes` is 8 per named argument, which have already
// consumed that much of the general register area; the next register then carries the va_list.
#[cfg(target_arch = "x86_64")]
macro_rules! variadic_entry {
    ($name:literal, $target:path, $named_bytes:literal, $va_list_register:literal) => {
        std::arch::global_asm!(
            concat!(".globl ", $name),
            concat!(".type ", $name, ", @function"),
            concat!($name, ":"),
            "sub rsp, 216",
            "mov [rsp + 40], rsi",
            "mov [rsp + 48], rdx",
            "mov [rsp + 56], rcx",
            "mov [rsp + 64], r8",
            "mov [rsp + 72], r9",
            "test al, al",
            "je 2f",
            "movaps [rsp + 80], xmm0",
            "movaps [rsp + 96], xmm1",
            "movaps [rsp + 112], xmm2",
            "movaps [rsp + 128], xmm3",
            "movaps [rsp + 144], xmm4",
            "movaps [rsp + 160], xmm5",
            "movaps [rsp + 176], xmm6",
            "movaps [rsp + 192], xmm7",
            "2:",
            concat!("mov dword ptr [rsp], ", $named_bytes),
            "mov dword ptr [rsp + 4], 48",
            "lea rax, [rsp + 224]",
            "mov [rsp + 8], rax",
            "lea rax, [rsp + 32]",
            "mov [rsp + 16], rax",
            concat!("mov ", $va_list_register, ", rsp"),
            "call {target}",
            "add rsp, 216",
            "ret",
            concat!(".size ", $name, ", . - ", $name),
            target = sym $target,
        );
    };
}

// The AAPCS64 form: the entry saves x0-x7 and q0-q7 in its frame and builds the 32-byte
// `va_list` there, `__stack` at the caller's stack arguments, `__gr_top` and `__vr_top` past each
// save area, and `__gr_offs` skipping the `$named_bytes` the named arguments took (no named
// argument is floating point). A `va_list` is passed by reference, so the next register gets its
// address. Frame: x29 and x30 at 0, general registers at 16, vector registers at 80, the
// `va_list` at 208, 240 bytes in all.
#[cfg(target_arch = "aarch64")]
macro_rules! variadic_entry {
    ($name:literal, $target:path, $named_bytes:literal, $va_list_register:literal) => {
        std::arch::global_asm!(
            concat!(".globl ", $name),
            concat!(".type ", $name, ", %function"),
            concat!($name, ":"),
            "sub sp, sp, #240",
            "stp x29, x30, [sp]",
            "mov x29, sp",
            "stp x0, x1, [sp, #16]",
            "stp x2, x3, [sp, #32]",
            "stp x4, x5, [sp, #48]",
            "stp x6, x7, [sp, #64]",
            "stp q0, q1, [sp, #80]",
            "stp q2, q3, [sp, #112]",
            "stp q4, q5, [sp, #144]",
            "stp q6, q7, [sp, #176]",
            "add x9, sp, #240",
            "str x9, [sp, #208]",
            "add x9, sp, #80",
            "str x9, [sp, #216]",
            "add x9, sp, #208",
            "str x9, [sp, #224]",
            concat!("mov w9, #(", $named_bytes, " - 64)"),
            "str w9, [sp, #232]",
            "mov w9, #-128",
            "str w9, [sp, #236]",
            concat!("add ", $va_list_register, ", sp, #208"),
            "bl {target}",
            "ldp x29, x30, [sp]",
            "add sp, sp, #240",
            "ret",
            concat!(".size ", $name, ", . - ", $name),
            target = sym $target,
        );
    };
}

/// The register after `$named` integer arguments, which carries the `va_list`.
macro_rules! variadic_entries {
    ($(($name:literal, $target:path, $named_bytes:literal, $x86_64:literal, $aarch64:literal),)*) => {
        $(
            #[cfg(target_arch = "x86_64")]
            variadic_entry!($name, $target, $named_bytes, $x86_64);
            #[cfg(target_arch = "aarch64")]
            variadic_entry!($name, $target, $named_bytes, $aarch64);
        )*
    };
}

variadic_entries! {
    ("uxdroid_fprintf", vfprintf, "16", "rdx", "x2"),
    ("uxdroid_fscanf", vfscanf, "16", "rdx", "x2"),
    ("uxdroid_fwprintf", vfwprintf, "16", "rdx", "x2"),
    ("uxdroid_fwscanf", vfwscanf, "16", "rdx", "x2"),
    ("uxdroid_printf", vprintf, "8", "rsi", "x1"),
    ("uxdroid_sprintf", vsprintf, "16", "rdx", "x2"),
    ("uxdroid_snprintf", checked_vsnprintf, "24", "rcx", "x3"),
    ("uxdroid_asprintf", vasprintf, "16", "rdx", "x2"),
    ("uxdroid_dprintf", vdprintf, "16", "rdx", "x2"),
    ("uxdroid_sscanf", vsscanf, "16", "rdx", "x2"),
    ("uxdroid_swprintf", vswprintf, "24", "rcx", "x3"),
    ("uxdroid_syslog", vsyslog, "16", "rdx", "x2"),
    ("uxdroid_sprintf_chk", vsprintf_chk, "32", "r8", "x4"),
    ("uxdroid_snprintf_chk", vsnprintf_chk, "40", "r9", "x5"),
}

// --- Formatted output ---

/// A `long double` argument travels in SSE registers on Android and in memory for glibc, so a
/// format that reads one would print garbage. Scanning costs little next to the formatting.
fn reject_long_double(format: impl Iterator<Item = u32>, function: &str) {
    const FLAGS_AND_WIDTH: &[u8] = b"-+ #0123456789.*'$";
    const FLOAT_CONVERSIONS: &[u8] = b"aAeEfFgG";
    let mut format = format.peekable();
    while let Some(unit) = format.next() {
        if unit != u32::from(b'%') {
            continue;
        }
        while format
            .next_if(|next| u8::try_from(*next).is_ok_and(|byte| FLAGS_AND_WIDTH.contains(&byte)))
            .is_some()
        {}
        if format.next_if_eq(&u32::from(b'L')).is_some()
            && format.peek().is_some_and(|conversion| {
                u8::try_from(*conversion).is_ok_and(|byte| FLOAT_CONVERSIONS.contains(&byte))
            })
        {
            die(format_args!(
                "uxdroid: {function} with a long double format has an incompatible ABI and no shim yet\n"
            ));
        }
        // Consumes `%%`, so its second `%` does not start a conversion.
        format.next_if_eq(&u32::from(b'%'));
    }
}

/// # Safety
/// `format` must be a C string.
pub(crate) unsafe fn check_format(format: *const c_char, function: &str) {
    // SAFETY: the caller promises a C string.
    let bytes = unsafe { CStr::from_ptr(format) }.to_bytes();
    reject_long_double(bytes.iter().map(|&byte| u32::from(byte)), function);
}

/// # Safety
/// `format` must be a NUL-terminated wide string.
unsafe fn check_wide_format(format: *const i32, function: &str) {
    let mut length = 0;
    // SAFETY: the caller promises a terminator.
    let units = unsafe {
        while *format.add(length) != 0 {
            length += 1;
        }
        std::slice::from_raw_parts(format, length)
    };
    reject_long_double(units.iter().map(|&unit| unit as u32), function);
}

unsafe extern "C" fn vprintf(format: *const c_char, args: *mut c_void) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vprintf");
        host_vprintf(format, args)
    }
}

unsafe extern "C" fn vsprintf(
    buffer: *mut c_char,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vsprintf");
        host_vsprintf(buffer, format, args)
    }
}

unsafe extern "C" fn checked_vsnprintf(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vsnprintf");
        vsnprintf(buffer, size, format, args)
    }
}

unsafe extern "C" fn vasprintf(
    buffer: *mut *mut c_char,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vasprintf");
        host_vasprintf(buffer, format, args)
    }
}

unsafe extern "C" fn vdprintf(fd: c_int, format: *const c_char, args: *mut c_void) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vdprintf");
        host_vdprintf(fd, format, args)
    }
}

unsafe extern "C" fn vsscanf(
    text: *const c_char,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vsscanf");
        host_vsscanf(text, format, args)
    }
}

unsafe extern "C" fn vswprintf(
    buffer: *mut i32,
    size: usize,
    format: *const i32,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_wide_format(format, "vswprintf");
        host_vswprintf(buffer, size, format, args)
    }
}

unsafe extern "C" fn vsyslog(priority: c_int, format: *const c_char, args: *mut c_void) {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vsyslog");
        host_vsyslog(priority, format, args);
    }
}

unsafe extern "C" fn vsprintf_chk(
    buffer: *mut c_char,
    flags: c_int,
    size: usize,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "__vsprintf_chk");
        __vsprintf_chk(buffer, flags, size, format, args)
    }
}

unsafe extern "C" fn vsnprintf_chk(
    buffer: *mut c_char,
    size: usize,
    flags: c_int,
    object_size: usize,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "__vsnprintf_chk");
        __vsnprintf_chk(buffer, size, flags, object_size, format, args)
    }
}

unsafe extern "C" fn vfprintf(
    file: *mut c_void,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vfprintf");
        stdio::vfprintf(file, format, args)
    }
}

unsafe extern "C" fn vfscanf(file: *mut c_void, format: *const c_char, args: *mut c_void) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_format(format, "vfscanf");
        stdio::vfscanf(file, format, args)
    }
}

unsafe extern "C" fn vfwprintf(file: *mut c_void, format: *const i32, args: *mut c_void) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_wide_format(format, "vfwprintf");
        stdio::vfwprintf(file, format, args)
    }
}

unsafe extern "C" fn vfwscanf(file: *mut c_void, format: *const i32, args: *mut c_void) -> c_int {
    // SAFETY: the C contract makes these valid.
    unsafe {
        check_wide_format(format, "vfwscanf");
        stdio::vfwscanf(file, format, args)
    }
}

// --- Non-local jumps ---

// bionic's jmp_buf is 88 bytes and glibc's 200, and glibc's `setjmp` writes a whole signal mask.
// These own the buffer: callee-saved registers at 0..56, the mask flag at 64, the mask at 72, and
// the floating point control words at 80.
#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".globl uxdroid_setjmp",
    ".globl uxdroid_underscore_setjmp",
    ".globl uxdroid_sigsetjmp",
    ".globl uxdroid_longjmp",
    "uxdroid_setjmp:",
    "mov esi, 1",
    "jmp uxdroid_sigsetjmp",
    "uxdroid_underscore_setjmp:",
    "xor esi, esi",
    "uxdroid_sigsetjmp:",
    "mov [rdi], rbx",
    "mov [rdi + 8], rbp",
    "mov [rdi + 16], r12",
    "mov [rdi + 24], r13",
    "mov [rdi + 32], r14",
    "mov [rdi + 40], r15",
    "lea rax, [rsp + 8]",
    "mov [rdi + 48], rax",
    "mov rax, [rsp]",
    "mov [rdi + 56], rax",
    "mov eax, esi",
    "mov [rdi + 64], rax",
    "stmxcsr [rdi + 80]",
    "fnstcw [rdi + 84]",
    "test esi, esi",
    "je 2f",
    "sub rsp, 8",
    "call {save_mask}",
    "add rsp, 8",
    "2:",
    "xor eax, eax",
    "ret",
    "uxdroid_longjmp:",
    "cmp qword ptr [rdi + 64], 0",
    "je 3f",
    "push rdi",
    "push rsi",
    "sub rsp, 8",
    "call {restore_mask}",
    "add rsp, 8",
    "pop rsi",
    "pop rdi",
    "3:",
    "ldmxcsr [rdi + 80]",
    "fldcw [rdi + 84]",
    "mov rbx, [rdi]",
    "mov rbp, [rdi + 8]",
    "mov r12, [rdi + 16]",
    "mov r13, [rdi + 24]",
    "mov r14, [rdi + 32]",
    "mov r15, [rdi + 40]",
    "mov eax, esi",
    "test eax, eax",
    "jne 4f",
    "mov eax, 1",
    "4:",
    "mov rsp, [rdi + 48]",
    "jmp qword ptr [rdi + 56]",
    save_mask = sym save_jump_mask,
    restore_mask = sym restore_jump_mask,
);

#[cfg(target_arch = "x86_64")]
const JUMP_MASK_OFFSET: usize = 72;

// On AArch64 bionic's jmp_buf is 256 bytes and glibc's 312. These own the buffer: x19-x30 at
// 0..96, sp at 96, d8-d15 at 104..168, the mask flag at 168, the mask at 176 and FPCR at 184.
#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".globl uxdroid_setjmp",
    ".globl uxdroid_underscore_setjmp",
    ".globl uxdroid_sigsetjmp",
    ".globl uxdroid_longjmp",
    "uxdroid_setjmp:",
    "mov w1, #1",
    "b uxdroid_sigsetjmp",
    "uxdroid_underscore_setjmp:",
    "mov w1, #0",
    "uxdroid_sigsetjmp:",
    "stp x19, x20, [x0]",
    "stp x21, x22, [x0, #16]",
    "stp x23, x24, [x0, #32]",
    "stp x25, x26, [x0, #48]",
    "stp x27, x28, [x0, #64]",
    "stp x29, x30, [x0, #80]",
    "mov x2, sp",
    "str x2, [x0, #96]",
    "stp d8, d9, [x0, #104]",
    "stp d10, d11, [x0, #120]",
    "stp d12, d13, [x0, #136]",
    "stp d14, d15, [x0, #152]",
    "mov w2, w1",
    "str x2, [x0, #168]",
    "mrs x2, fpcr",
    "str x2, [x0, #184]",
    "cbz w1, 2f",
    "stp x29, x30, [sp, #-16]!",
    "mov x29, sp",
    "bl {save_mask}",
    "ldp x29, x30, [sp], #16",
    "2:",
    "mov w0, #0",
    "ret",
    "uxdroid_longjmp:",
    "ldr x2, [x0, #168]",
    "cbz x2, 3f",
    "stp x0, x1, [sp, #-16]!",
    "stp x29, x30, [sp, #-16]!",
    "mov x29, sp",
    "bl {restore_mask}",
    "ldp x29, x30, [sp], #16",
    "ldp x0, x1, [sp], #16",
    "3:",
    "ldr x2, [x0, #184]",
    "msr fpcr, x2",
    "ldp x19, x20, [x0]",
    "ldp x21, x22, [x0, #16]",
    "ldp x23, x24, [x0, #32]",
    "ldp x25, x26, [x0, #48]",
    "ldp x27, x28, [x0, #64]",
    "ldp x29, x30, [x0, #80]",
    "ldp d8, d9, [x0, #104]",
    "ldp d10, d11, [x0, #120]",
    "ldp d12, d13, [x0, #136]",
    "ldp d14, d15, [x0, #152]",
    "ldr x2, [x0, #96]",
    "mov sp, x2",
    "cmp w1, #0",
    "csinc w0, w1, wzr, ne",
    "ret",
    save_mask = sym save_jump_mask,
    restore_mask = sym restore_jump_mask,
);

#[cfg(target_arch = "aarch64")]
const JUMP_MASK_OFFSET: usize = 176;

unsafe extern "C" fn save_jump_mask(buffer: *mut u8) {
    // SAFETY: zero is a valid set, and `buffer` is the caller's jmp_buf, which holds this offset.
    unsafe {
        let mut current: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut current);
        buffer
            .add(JUMP_MASK_OFFSET)
            .cast::<u64>()
            .write_unaligned(narrow(&current));
    }
}

unsafe extern "C" fn restore_jump_mask(buffer: *const u8) {
    // SAFETY: `buffer` is the caller's jmp_buf, which `save_jump_mask` filled.
    unsafe {
        let wide = widen(buffer.add(JUMP_MASK_OFFSET).cast::<u64>());
        libc::pthread_sigmask(libc::SIG_SETMASK, &wide, std::ptr::null_mut());
    }
}

// --- Path helpers ---

thread_local! {
    /// bionic's `basename` and `dirname` return a per-thread buffer and leave their input alone.
    static PATH_RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn path_result(text: &[u8]) -> *mut c_char {
    PATH_RESULT.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        buffer.clear();
        buffer.extend_from_slice(text);
        buffer.push(0);
        buffer.as_mut_ptr().cast()
    })
}

fn path_base(path: &[u8]) -> &[u8] {
    let trimmed = path.trim_ascii_end_slashes();
    if path.is_empty() {
        return b".";
    }
    if trimmed.is_empty() {
        return b"/";
    }
    let start = trimmed
        .iter()
        .rposition(|&byte| byte == b'/')
        .map_or(0, |slash| slash + 1);
    &trimmed[start..]
}

fn path_dir(path: &[u8]) -> &[u8] {
    if path.is_empty() {
        return b".";
    }
    let trimmed = path.trim_ascii_end_slashes();
    if trimmed.is_empty() {
        return b"/";
    }
    let Some(slash) = trimmed.iter().rposition(|&byte| byte == b'/') else {
        return b".";
    };
    let parent = trimmed[..slash].trim_ascii_end_slashes();
    if parent.is_empty() { b"/" } else { parent }
}

trait TrimSlashes {
    fn trim_ascii_end_slashes(&self) -> &[u8];
}

impl TrimSlashes for [u8] {
    fn trim_ascii_end_slashes(&self) -> &[u8] {
        let end = self
            .iter()
            .rposition(|&byte| byte != b'/')
            .map_or(0, |last| last + 1);
        &self[..end]
    }
}

/// # Safety
/// `path` must be null or a C string.
unsafe fn path_bytes<'a>(path: *const c_char) -> &'a [u8] {
    if path.is_null() {
        return b"";
    }
    // SAFETY: not null, and the caller promises a C string.
    unsafe { CStr::from_ptr(path) }.to_bytes()
}

unsafe extern "C" fn basename(path: *const c_char) -> *mut c_char {
    // SAFETY: the C contract makes `path` null or a C string.
    path_result(path_base(unsafe { path_bytes(path) }))
}

unsafe extern "C" fn dirname(path: *const c_char) -> *mut c_char {
    // SAFETY: the C contract makes `path` null or a C string.
    path_result(path_dir(unsafe { path_bytes(path) }))
}

// --- Stream calls with another shape ---

const BIONIC_BUFSIZ: usize = 1024;

unsafe extern "C" fn setbuf(file: *mut c_void, buffer: *mut c_char) {
    // glibc's own `setbuf` assumes its 8192-byte BUFSIZ and would overrun bionic's 1024.
    let mode = if buffer.is_null() {
        libc::_IONBF
    } else {
        libc::_IOFBF
    };
    // SAFETY: the C contract gives `buffer` BUFSIZ bytes, or makes it null.
    unsafe { stdio::setvbuf(file, buffer, mode, BIONIC_BUFSIZ) };
}

fn check_stream_size(function: &str, size: usize, count: usize, object_size: usize) {
    if size
        .checked_mul(count)
        .is_none_or(|total| total > object_size)
    {
        fortify_failure(function);
    }
}

unsafe extern "C" fn fread_chk(
    buffer: *mut c_void,
    size: usize,
    count: usize,
    file: *mut c_void,
    object_size: usize,
) -> usize {
    check_stream_size("fread", size, count, object_size);
    // SAFETY: the check proves the read fits the caller's object.
    unsafe { stdio::fread(buffer, size, count, file) }
}

unsafe extern "C" fn fwrite_chk(
    buffer: *const c_void,
    size: usize,
    count: usize,
    file: *mut c_void,
    object_size: usize,
) -> usize {
    check_stream_size("fwrite", size, count, object_size);
    // SAFETY: the check proves the write stays inside the caller's object.
    unsafe { stdio::fwrite(buffer, size, count, file) }
}

unsafe extern "C" fn fgets_chk(
    buffer: *mut c_char,
    size: c_int,
    file: *mut c_void,
    object_size: usize,
) -> *mut c_char {
    if size < 0 || size as usize > object_size {
        fortify_failure("fgets");
    }
    // SAFETY: the check proves the read fits the caller's object.
    unsafe { stdio::fgets(buffer, size, file) }
}

// --- Name lookups and limits ---

/// bionic's `_PC_*` values, from `unistd.h`, mapped to glibc's.
const PATHCONF_NAMES: &[(c_int, c_int)] = &[
    (0, libc::_PC_FILESIZEBITS),
    (1, libc::_PC_LINK_MAX),
    (2, libc::_PC_MAX_CANON),
    (3, libc::_PC_MAX_INPUT),
    (4, libc::_PC_NAME_MAX),
    (5, libc::_PC_PATH_MAX),
    (6, libc::_PC_PIPE_BUF),
    (7, libc::_PC_2_SYMLINKS),
    (8, libc::_PC_ALLOC_SIZE_MIN),
    (9, libc::_PC_REC_INCR_XFER_SIZE),
    (10, libc::_PC_REC_MAX_XFER_SIZE),
    (11, libc::_PC_REC_MIN_XFER_SIZE),
    (12, libc::_PC_REC_XFER_ALIGN),
    (13, libc::_PC_SYMLINK_MAX),
    (14, libc::_PC_CHOWN_RESTRICTED),
    (15, libc::_PC_NO_TRUNC),
    (16, libc::_PC_VDISABLE),
    (17, libc::_PC_ASYNC_IO),
    (18, libc::_PC_PRIO_IO),
    (19, libc::_PC_SYNC_IO),
];

fn host_pathconf_name(name: c_int) -> Option<c_int> {
    let host = PATHCONF_NAMES.iter().find(|(bionic, _)| *bionic == name);
    if host.is_none() {
        set_errno(libc::EINVAL);
    }
    host.map(|(_, glibc)| *glibc)
}

unsafe extern "C" fn pathconf(path: *const c_char, name: c_int) -> c_long {
    match host_pathconf_name(name) {
        // SAFETY: the C contract makes `path` a C string.
        Some(host) => unsafe { libc::pathconf(path, host) },
        None => -1,
    }
}

unsafe extern "C" fn fpathconf(fd: c_int, name: c_int) -> c_long {
    match host_pathconf_name(name) {
        // SAFETY: pathconf with a descriptor takes only integers.
        Some(host) => unsafe { libc::fpathconf(fd, host) },
        None => -1,
    }
}

/// bionic's `NI_*` flags, from `netdb.h`, mapped to glibc's.
const NI_FLAGS: &[(c_int, c_int)] = &[
    (0x01, libc::NI_NOFQDN),
    (0x02, libc::NI_NUMERICHOST),
    (0x04, libc::NI_NAMEREQD),
    (0x08, libc::NI_NUMERICSERV),
    (0x10, libc::NI_DGRAM),
];

unsafe extern "C" fn getnameinfo(
    address: *const libc::sockaddr,
    address_size: libc::socklen_t,
    host: *mut c_char,
    host_size: usize,
    service: *mut c_char,
    service_size: usize,
    flags: c_int,
) -> c_int {
    let (Ok(host_size), Ok(service_size)) = (host_size.try_into(), service_size.try_into()) else {
        return bionic_eai(libc::EAI_OVERFLOW);
    };
    let mut host_flags = 0;
    for &(bionic, glibc) in NI_FLAGS {
        if flags & bionic != 0 {
            host_flags |= glibc;
        }
    }
    // SAFETY: the C contract makes these valid for the sizes given.
    let status = unsafe {
        libc::getnameinfo(
            address,
            address_size,
            host,
            host_size,
            service,
            service_size,
            host_flags,
        )
    };
    if status == 0 { 0 } else { bionic_eai(status) }
}

fn bionic_eai(status: c_int) -> c_int {
    EAI_CODES
        .iter()
        .find(|(_, glibc)| *glibc == status)
        .map_or(status.abs(), |(bionic, _)| *bionic)
}

// --- Standard streams ---

/// Libraries built for API levels below 23 reach the standard streams as `&__sF[n]`, an array
/// with bionic's 152-byte stride. glibc's 216-byte FILE cannot be laid out that way, so `__sF` is
/// only address space, and every function taking a stream translates addresses inside it.
const BIONIC_FILE_SIZE: usize = 152;
const STANDARD_STREAMS: usize = 3;

#[repr(align(16))]
struct LegacyStreams(#[allow(dead_code)] [u8; BIONIC_FILE_SIZE * STANDARD_STREAMS]);

static LEGACY_STREAMS: LegacyStreams = LegacyStreams([0; BIONIC_FILE_SIZE * STANDARD_STREAMS]);

unsafe extern "C" {
    static stdin: *mut c_void;
    static stdout: *mut c_void;
    static stderr: *mut c_void;
}

fn host_stream(file: *mut c_void) -> *mut c_void {
    let offset = (file as usize).wrapping_sub(std::ptr::from_ref(&LEGACY_STREAMS) as usize);
    // SAFETY: glibc initializes these variables before any library can run.
    unsafe {
        match offset / BIONIC_FILE_SIZE {
            0 => stdin,
            1 => stdout,
            2 => stderr,
            _ => file,
        }
    }
}

macro_rules! stream_functions {
    ($($name:ident($($before:ident: $before_type:ty),*; $($after:ident: $after_type:ty),*) -> $result:ty;)*) => {
        mod host_stdio {
            #[allow(unused_imports)]
            use super::*;
            unsafe extern "C" {
                $(pub fn $name($($before: $before_type,)* file: *mut c_void $(, $after: $after_type)*) -> $result;)*
            }
        }
        mod stdio {
            #[allow(unused_imports)]
            use super::*;
            $(
                /// # Safety
                /// Same contract as the C function of this name.
                pub unsafe extern "C" fn $name($($before: $before_type,)* file: *mut c_void $(, $after: $after_type)*) -> $result {
                    // SAFETY: forwards the caller's arguments with the stream translated.
                    unsafe { host_stdio::$name($($before,)* host_stream(file) $(, $after)*) }
                }
            )*
        }
        const STREAM_FUNCTIONS: &[(&str, *const ())] = &[
            $((stringify!($name), stdio::$name as *const ()),)*
        ];
    };
}

stream_functions! {
    clearerr(;) -> ();
    fclose(;) -> c_int;
    feof(;) -> c_int;
    ferror(;) -> c_int;
    fflush(;) -> c_int;
    fgetc(;) -> c_int;
    fgets(text: *mut c_char, size: c_int;) -> *mut c_char;
    fgetwc(;) -> u32;
    fgetws(text: *mut i32, size: c_int;) -> *mut i32;
    fileno(;) -> c_int;
    flockfile(;) -> ();
    fputc(character: c_int;) -> c_int;
    fputs(text: *const c_char;) -> c_int;
    fputwc(character: i32;) -> u32;
    fputws(text: *const i32;) -> c_int;
    fread(buffer: *mut c_void, size: usize, count: usize;) -> usize;
    freopen(path: *const c_char, mode: *const c_char;) -> *mut c_void;
    fseek(; offset: c_long, whence: c_int) -> c_int;
    fseeko(; offset: i64, whence: c_int) -> c_int;
    ftell(;) -> c_long;
    ftello(;) -> i64;
    ftrylockfile(;) -> c_int;
    funlockfile(;) -> ();
    fwide(; mode: c_int) -> c_int;
    fwrite(buffer: *const c_void, size: usize, count: usize;) -> usize;
    getc(;) -> c_int;
    getc_unlocked(;) -> c_int;
    getdelim(line: *mut *mut c_char, size: *mut usize, delimiter: c_int;) -> isize;
    getline(line: *mut *mut c_char, size: *mut usize;) -> isize;
    getwc(;) -> u32;
    putc(character: c_int;) -> c_int;
    putc_unlocked(character: c_int;) -> c_int;
    putwc(character: i32;) -> u32;
    rewind(;) -> ();
    setbuffer(; buffer: *mut c_char, size: usize) -> ();
    setlinebuf(;) -> ();
    setvbuf(; buffer: *mut c_char, mode: c_int, size: usize) -> c_int;
    ungetc(character: c_int;) -> c_int;
    ungetwc(character: u32;) -> u32;
    vfprintf(; format: *const c_char, args: *mut c_void) -> c_int;
    vfscanf(; format: *const c_char, args: *mut c_void) -> c_int;
    vfwprintf(; format: *const i32, args: *mut c_void) -> c_int;
    vfwscanf(; format: *const i32, args: *mut c_void) -> c_int;
}

// --- funopen ---

type FunRead = unsafe extern "C" fn(*mut c_void, *mut c_char, c_int) -> c_int;
type FunWrite = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int;
/// bionic's `fpos_t` and `fpos64_t` are both `off64_t` on LP64.
type FunSeek = unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64;
type FunClose = unsafe extern "C" fn(*mut c_void) -> c_int;

/// glibc's `cookie_io_functions_t`.
#[repr(C)]
struct CookieFunctions {
    read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> isize>,
    write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> isize>,
    seek: Option<unsafe extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int>,
    close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

unsafe extern "C" {
    fn fopencookie(
        cookie: *mut c_void,
        mode: *const c_char,
        functions: CookieFunctions,
    ) -> *mut c_void;
}

/// The BSD callbacks behind one glibc cookie stream.
struct FunCookie {
    cookie: *mut c_void,
    read: Option<FunRead>,
    write: Option<FunWrite>,
    seek: Option<FunSeek>,
    close: Option<FunClose>,
}

/// BSD's custom streams, which glibc offers as `fopencookie` with other callback types. Libraries
/// such as Meta's Superpack decompressor read their archives through them.
unsafe extern "C" fn funopen(
    cookie: *const c_void,
    read: Option<FunRead>,
    write: Option<FunWrite>,
    seek: Option<FunSeek>,
    close: Option<FunClose>,
) -> *mut c_void {
    let mode = match (read.is_some(), write.is_some()) {
        (true, true) => c"r+",
        (true, false) => c"r",
        (false, true) => c"w",
        (false, false) => {
            // SAFETY: errno is this thread's.
            unsafe { *libc::__errno_location() = libc::EINVAL };
            return std::ptr::null_mut();
        }
    };
    let state = Box::into_raw(Box::new(FunCookie {
        cookie: cookie.cast_mut(),
        read,
        write,
        seek,
        close,
    }));
    let functions = CookieFunctions {
        read: read.map(|_| fun_read as _),
        write: write.map(|_| fun_write as _),
        seek: seek.map(|_| fun_seek as _),
        close: Some(fun_close),
    };
    // SAFETY: the mode is a valid C string, and `state` lives until `fun_close` frees it.
    let file = unsafe { fopencookie(state.cast(), mode.as_ptr(), functions) };
    if file.is_null() {
        // SAFETY: glibc did not keep `state`.
        drop(unsafe { Box::from_raw(state) });
    }
    file
}

/// BSD callbacks take an `int` count, so a larger request is served in part, as a short read is.
fn fun_count(size: usize) -> c_int {
    size.min(c_int::MAX as usize) as c_int
}

unsafe extern "C" fn fun_read(state: *mut c_void, buffer: *mut c_char, size: usize) -> isize {
    // SAFETY: glibc passes back the state `funopen` gave it, which is live until close.
    let state = unsafe { &*state.cast::<FunCookie>() };
    // SAFETY: only set for a stream opened with a read callback, given the caller's buffer.
    unsafe { state.read.expect("read callback")(state.cookie, buffer, fun_count(size)) as isize }
}

unsafe extern "C" fn fun_write(state: *mut c_void, buffer: *const c_char, size: usize) -> isize {
    // SAFETY: as in `fun_read`.
    let state = unsafe { &*state.cast::<FunCookie>() };
    // SAFETY: only set for a stream opened with a write callback, given the caller's buffer.
    let written =
        unsafe { state.write.expect("write callback")(state.cookie, buffer, fun_count(size)) };
    // glibc reads a write of 0 as an error, as BSD reads -1.
    written.max(0) as isize
}

unsafe extern "C" fn fun_seek(state: *mut c_void, offset: *mut i64, whence: c_int) -> c_int {
    // SAFETY: as in `fun_read`; glibc gives a valid offset to update.
    unsafe {
        let state = &*state.cast::<FunCookie>();
        let position = state.seek.expect("seek callback")(state.cookie, *offset, whence);
        if position < 0 {
            return -1;
        }
        *offset = position;
    }
    0
}

unsafe extern "C" fn fun_close(state: *mut c_void) -> c_int {
    // SAFETY: glibc calls close once, last, so the state is freed here.
    let state = unsafe { Box::from_raw(state.cast::<FunCookie>()) };
    // SAFETY: the callback is the library's own, given its cookie.
    state
        .close
        .map_or(0, |close| unsafe { close(state.cookie) })
}

/// # Safety
/// Strings must be null or valid C strings.
pub(crate) unsafe fn lossy<'a>(text: *const c_char) -> std::borrow::Cow<'a, str> {
    if text.is_null() {
        return "".into();
    }
    // SAFETY: the caller passes a valid C string.
    unsafe { CStr::from_ptr(text) }.to_string_lossy()
}

unsafe extern "C" fn assert_failed(file: *const c_char, line: c_int, message: *const c_char) -> ! {
    // SAFETY: the compiler generates these C strings for `assert`.
    let (file, message) = unsafe { (lossy(file), lossy(message)) };
    die(format_args!(
        "{file}:{line}: assertion \"{message}\" failed\n"
    ))
}

unsafe extern "C" fn assert2_failed(
    file: *const c_char,
    line: c_int,
    function: *const c_char,
    message: *const c_char,
) -> ! {
    // SAFETY: the compiler generates these C strings for `assert`.
    let (file, function, message) = unsafe { (lossy(file), lossy(function), lossy(message)) };
    die(format_args!(
        "{file}:{line}: {function}: assertion \"{message}\" failed\n"
    ))
}

unsafe extern "C" fn set_abort_message(message: *const c_char) {
    // SAFETY: the NDK contract makes this a C string.
    let message = unsafe { lossy(message) };
    write_stderr(format_args!("abort message: {message}\n"));
}

// --- System properties ---

const PROP_VALUE_MAX: usize = 92;

unsafe extern "C" fn system_property_get(name: *const c_char, value: *mut c_char) -> c_int {
    // SAFETY: the NDK contract makes `name` a C string and gives `value` PROP_VALUE_MAX bytes.
    unsafe {
        let text: &[u8] = if CStr::from_ptr(name) == c"ro.build.version.sdk" {
            API_LEVEL.to_bytes_with_nul()
        } else {
            b"\0"
        };
        let length = text.len().min(PROP_VALUE_MAX);
        std::ptr::copy_nonoverlapping(text.as_ptr().cast(), value, length);
        (length - 1) as c_int
    }
}

extern "C" fn system_property_find(_name: *const c_char) -> *const c_void {
    std::ptr::null()
}

/// No property is ever found, so no caller holds a real one; an empty name and value are safe.
unsafe extern "C" fn system_property_read(
    _property: *const c_void,
    name: *mut c_char,
    value: *mut c_char,
) -> c_int {
    // SAFETY: bionic's contract gives each non-null buffer room for at least its terminator.
    unsafe {
        if !name.is_null() {
            *name = 0;
        }
        if !value.is_null() {
            *value = 0;
        }
    }
    0
}

extern "C" fn system_property_read_callback(
    _property: *const c_void,
    _callback: *const c_void,
    _cookie: *mut c_void,
) {
}

/// There are no properties to visit.
extern "C" fn system_property_foreach(_callback: *const c_void, _cookie: *mut c_void) -> c_int {
    0
}

extern "C" fn system_property_find_nth(_index: u32) -> *const c_void {
    std::ptr::null()
}

/// No property is ever found, so no caller holds one to ask about.
extern "C" fn system_property_serial(_property: *const c_void) -> u32 {
    0
}

/// The serial of the property area, which never changes: nothing is ever set.
const PROPERTY_AREA_SERIAL: u32 = 0;

extern "C" fn system_property_area_serial() -> u32 {
    PROPERTY_AREA_SERIAL
}

/// There is no property service to take the value, as when bionic cannot reach it.
extern "C" fn system_property_set(_name: *const c_char, _value: *const c_char) -> c_int {
    -1
}

/// Waits for the area's serial, or a property's, to differ from `old_serial`. Nothing changes, so
/// a serial that already differs returns at once and any other waits out the relative timeout,
/// forever without one, as bionic's futex wait would.
unsafe extern "C" fn system_property_wait(
    property: *const c_void,
    old_serial: u32,
    new_serial: *mut u32,
    timeout: *const libc::timespec,
) -> bool {
    let serial = match property.is_null() {
        true => PROPERTY_AREA_SERIAL,
        false => 0,
    };
    if serial != old_serial {
        if !new_serial.is_null() {
            // SAFETY: bionic's contract makes a non-null `new_serial` writable.
            unsafe { *new_serial = serial };
        }
        return true;
    }
    // SAFETY: a non-null timeout is the caller's valid timespec.
    sleep_out(unsafe { timeout.as_ref() });
    false
}

/// The deprecated form: waits for the area's serial to differ from `old_serial`, with no timeout.
extern "C" fn system_property_wait_any(old_serial: u32) -> u32 {
    if old_serial != PROPERTY_AREA_SERIAL {
        return PROPERTY_AREA_SERIAL;
    }
    sleep_out(None);
    unreachable!("an endless wait returned")
}

/// Sleeps for `timeout`, through signals, or forever with none.
fn sleep_out(timeout: Option<&libc::timespec>) {
    let Some(timeout) = timeout else {
        loop {
            // SAFETY: pause has no preconditions.
            unsafe { libc::pause() };
        }
    };
    let mut left = *timeout;
    // SAFETY: both pointers are to this frame's timespec, which nanosleep reads and updates.
    while unsafe { libc::nanosleep(&left, &mut left) } != 0 && errno() == libc::EINTR {}
}

fn errno() -> c_int {
    // SAFETY: errno is this thread's.
    unsafe { *libc::__errno_location() }
}

// --- BSD ctype ---

const CTYPE_U: u8 = 0x01;
const CTYPE_L: u8 = 0x02;
const CTYPE_N: u8 = 0x04;
const CTYPE_S: u8 = 0x08;
const CTYPE_P: u8 = 0x10;
const CTYPE_C: u8 = 0x20;
const CTYPE_X: u8 = 0x40;
const CTYPE_B: u8 = 0x80;

/// bionic's `_C_ctype_`, from OpenBSD: one entry for EOF, then the class bits of each byte in the
/// C locale. Old NDK headers expand `isalpha` and friends to `(_ctype_ + 1)[c]`.
const fn bsd_ctype() -> [u8; 257] {
    let mut table = [0u8; 257];
    let mut c = 0;
    while c < 128 {
        let byte = c as u8;
        table[1 + c] = match byte {
            b'\t'..=b'\r' => CTYPE_C | CTYPE_S,
            0..=31 | 127 => CTYPE_C,
            b' ' => CTYPE_S | CTYPE_B,
            b'0'..=b'9' => CTYPE_N,
            b'A'..=b'F' => CTYPE_U | CTYPE_X,
            b'G'..=b'Z' => CTYPE_U,
            b'a'..=b'f' => CTYPE_L | CTYPE_X,
            b'g'..=b'z' => CTYPE_L,
            _ => CTYPE_P,
        };
        c += 1;
    }
    table
}

static BSD_CTYPE: [u8; 257] = bsd_ctype();

/// The data symbol `_ctype_`: a `const char*`, not the table itself.
#[repr(transparent)]
struct CtypePointer(*const u8);
// SAFETY: it points at an immutable static.
unsafe impl Sync for CtypePointer {}

static CTYPE: CtypePointer = CtypePointer(BSD_CTYPE.as_ptr());

// --- Threads ---

/// bionic's `pthread_gettid_np`. glibc 2.42 has it; older glibc keeps the thread ID at one offset
/// in every thread's descriptor, found once by looking for this thread's own ID in its own.
unsafe extern "C" fn pthread_gettid_np(thread: libc::pthread_t) -> libc::pid_t {
    type Host = unsafe extern "C" fn(libc::pthread_t) -> libc::pid_t;
    static HOST: OnceLock<Option<Host>> = OnceLock::new();
    static OFFSET: OnceLock<Option<usize>> = OnceLock::new();
    let host = HOST.get_or_init(|| {
        // SAFETY: a glibc function of this name has bionic's signature.
        host_symbol(c"pthread_gettid_np").map(|address| unsafe { std::mem::transmute(address) })
    });
    if let Some(host) = host {
        // SAFETY: the caller passes a live thread.
        return unsafe { host(thread) };
    }
    // SAFETY: pthread_self and gettid have no preconditions.
    let (this, tid) = unsafe { (libc::pthread_self(), libc::gettid()) };
    if thread == this {
        return tid;
    }
    let offset = OFFSET.get_or_init(|| {
        (0..1024).step_by(4).find(|&offset| {
            // SAFETY: glibc's descriptor of this thread is larger than 1 KiB and readable.
            unsafe { ((this as usize + offset) as *const libc::pid_t).read() == tid }
        })
    });
    match offset {
        // SAFETY: the caller passes a live thread, whose descriptor has the same layout.
        Some(offset) => unsafe { ((thread as usize + offset) as *const libc::pid_t).read() },
        None => -1,
    }
}

// --- Fortify ---

unsafe extern "C" fn write_chk(
    fd: c_int,
    buffer: *const c_void,
    count: usize,
    object_size: usize,
) -> isize {
    if count > object_size {
        fortify_failure("write");
    }
    // SAFETY: the check proves the write stays inside the caller's object.
    unsafe { libc::write(fd, buffer, count) }
}

/// bionic's `off_t` is 64-bit on LP64, so `__pwrite_chk` and `__pwrite64_chk` are one function, which
/// glibc lacks under either name.
unsafe extern "C" fn pwrite_chk(
    fd: c_int,
    buffer: *const c_void,
    count: usize,
    offset: i64,
    object_size: usize,
) -> isize {
    if count > object_size {
        fortify_failure("pwrite");
    }
    // SAFETY: the check proves the write stays inside the caller's object.
    unsafe { libc::pwrite64(fd, buffer, count, offset) }
}

extern "C" fn umask_chk(mode: libc::mode_t) -> libc::mode_t {
    if mode & !0o777 != 0 {
        die(format_args!(
            "uxdroid: umask: called with invalid mask {mode:o}\n"
        ));
    }
    // SAFETY: umask accepts any mask.
    unsafe { libc::umask(mode) }
}

unsafe extern "C" fn strlen_chk(text: *const c_char, object_size: usize) -> usize {
    // SAFETY: the compiler passes the object's size, so reading that far is in bounds.
    let length = unsafe { libc::strnlen(text, object_size) };
    if length >= object_size {
        fortify_failure("strlen");
    }
    length
}

unsafe extern "C" fn strchr_chk(
    text: *const c_char,
    character: c_int,
    object_size: usize,
) -> *mut c_char {
    let wanted = character as c_char;
    for index in 0..object_size {
        // SAFETY: `index` stays inside the object, whose size the compiler passed.
        let (address, unit) = unsafe { (text.add(index), *text.add(index)) };
        if unit == wanted {
            return address.cast_mut();
        }
        if unit == 0 {
            return std::ptr::null_mut();
        }
    }
    fortify_failure("strchr")
}

unsafe extern "C" fn strrchr_chk(
    text: *const c_char,
    character: c_int,
    object_size: usize,
) -> *mut c_char {
    let wanted = character as c_char;
    let mut last = std::ptr::null_mut();
    for index in 0..object_size {
        // SAFETY: `index` stays inside the object, whose size the compiler passed.
        let (address, unit) = unsafe { (text.add(index), *text.add(index)) };
        if unit == wanted {
            last = address.cast_mut();
        }
        if unit == 0 {
            return last;
        }
    }
    fortify_failure("strrchr")
}

unsafe extern "C" fn strlcpy_chk(
    destination: *mut c_char,
    source: *const c_char,
    size: usize,
    object_size: usize,
) -> usize {
    if size > object_size {
        fortify_failure("strlcpy");
    }
    // SAFETY: `size` fits the destination, and the source is a C string.
    unsafe {
        let length = libc::strlen(source);
        if size > 0 {
            let copied = length.min(size - 1);
            std::ptr::copy_nonoverlapping(source, destination, copied);
            *destination.add(copied) = 0;
        }
        length
    }
}

unsafe extern "C" fn strlcat_chk(
    destination: *mut c_char,
    source: *const c_char,
    size: usize,
    object_size: usize,
) -> usize {
    if size > object_size {
        fortify_failure("strlcat");
    }
    // SAFETY: `size` fits the destination, and the source is a C string.
    unsafe {
        let used = libc::strnlen(destination, size);
        used + strlcpy_chk(destination.add(used), source, size - used, size - used)
    }
}

unsafe extern "C" fn strncpy_chk2(
    destination: *mut c_char,
    source: *const c_char,
    count: usize,
    destination_size: usize,
    source_size: usize,
) -> *mut c_char {
    if count > destination_size {
        fortify_failure("strncpy");
    }
    // SAFETY: `count` fits the destination, and the read stops at the source's known size.
    unsafe {
        let copied = libc::strnlen(source, count.min(source_size));
        if copied == source_size && copied < count {
            fortify_failure("strncpy");
        }
        std::ptr::copy_nonoverlapping(source, destination, copied);
        std::ptr::write_bytes(destination.add(copied), 0, count - copied);
    }
    destination
}

const FD_SETSIZE: c_int = 1024;
const FD_WORD_BITS: usize = 64;

fn check_fd_set(fd: c_int, set_size: usize) {
    if !(0..FD_SETSIZE).contains(&fd) || set_size < (FD_SETSIZE as usize) / 8 {
        fortify_failure("FD_SET");
    }
}

unsafe extern "C" fn fd_set_chk(fd: c_int, set: *mut u64, set_size: usize) {
    check_fd_set(fd, set_size);
    // SAFETY: the check proves `fd` indexes inside a full fd_set.
    unsafe { *set.add(fd as usize / FD_WORD_BITS) |= 1 << (fd as usize % FD_WORD_BITS) };
}

unsafe extern "C" fn fd_clr_chk(fd: c_int, set: *mut u64, set_size: usize) {
    check_fd_set(fd, set_size);
    // SAFETY: the check proves `fd` indexes inside a full fd_set.
    unsafe { *set.add(fd as usize / FD_WORD_BITS) &= !(1 << (fd as usize % FD_WORD_BITS)) };
}

unsafe extern "C" fn fd_isset_chk(fd: c_int, set: *const u64, set_size: usize) -> c_int {
    check_fd_set(fd, set_size);
    // SAFETY: the check proves `fd` indexes inside a full fd_set.
    let word = unsafe { *set.add(fd as usize / FD_WORD_BITS) };
    ((word >> (fd as usize % FD_WORD_BITS)) & 1) as c_int
}

// --- Signals ---

const MAX_SIGNAL: c_int = 64;

/// Widens bionic's 8-byte set into glibc's 128-byte one. The kernel only reads the first word.
///
/// # Safety
/// `set` must point to at least 8 readable bytes.
unsafe fn widen(set: *const u64) -> libc::sigset_t {
    // SAFETY: zero is a valid sigset_t, and it is larger than the word written at its start.
    unsafe {
        let mut wide: libc::sigset_t = std::mem::zeroed();
        std::ptr::write_unaligned(
            std::ptr::from_mut(&mut wide).cast::<u64>(),
            set.read_unaligned(),
        );
        wide
    }
}

fn narrow(wide: &libc::sigset_t) -> u64 {
    // SAFETY: sigset_t starts with this word.
    unsafe { std::ptr::read_unaligned(std::ptr::from_ref(wide).cast::<u64>()) }
}

/// `wide` paired with the pointer to pass along, keeping a null set null.
///
/// # Safety
/// `set` must be null or point to 8 readable bytes.
unsafe fn widen_optional(set: *const u64) -> (libc::sigset_t, bool) {
    if set.is_null() {
        // SAFETY: zero is a valid sigset_t.
        (unsafe { std::mem::zeroed() }, false)
    } else {
        // SAFETY: not null, and the caller promises 8 readable bytes.
        (unsafe { widen(set) }, true)
    }
}

fn pointer_if(wide: &libc::sigset_t, present: bool) -> *const libc::sigset_t {
    if present { wide } else { std::ptr::null() }
}

fn set_errno(value: c_int) {
    // SAFETY: glibc returns a valid pointer to this thread's errno.
    unsafe { *libc::__errno_location() = value };
}

fn signal_bit(signal: c_int) -> Option<u64> {
    (1..=MAX_SIGNAL)
        .contains(&signal)
        .then(|| 1u64 << (signal - 1))
}

unsafe extern "C" fn sigemptyset(set: *mut u64) -> c_int {
    // SAFETY: bionic's sigset_t is these 8 bytes.
    unsafe { set.write_unaligned(0) };
    0
}

unsafe extern "C" fn sigfillset(set: *mut u64) -> c_int {
    // SAFETY: bionic's sigset_t is these 8 bytes.
    unsafe { set.write_unaligned(u64::MAX) };
    0
}

unsafe extern "C" fn sigaddset(set: *mut u64, signal: c_int) -> c_int {
    let Some(bit) = signal_bit(signal) else {
        set_errno(libc::EINVAL);
        return -1;
    };
    // SAFETY: bionic's sigset_t is these 8 bytes.
    unsafe { set.write_unaligned(set.read_unaligned() | bit) };
    0
}

unsafe extern "C" fn sigdelset(set: *mut u64, signal: c_int) -> c_int {
    let Some(bit) = signal_bit(signal) else {
        set_errno(libc::EINVAL);
        return -1;
    };
    // SAFETY: bionic's sigset_t is these 8 bytes.
    unsafe { set.write_unaligned(set.read_unaligned() & !bit) };
    0
}

unsafe extern "C" fn sigismember(set: *const u64, signal: c_int) -> c_int {
    let Some(bit) = signal_bit(signal) else {
        set_errno(libc::EINVAL);
        return -1;
    };
    // SAFETY: bionic's sigset_t is these 8 bytes.
    c_int::from(unsafe { set.read_unaligned() } & bit != 0)
}

/// Layout measured from bionic's headers: flags first, unlike glibc.
#[repr(C)]
struct BionicSigaction {
    flags: c_int,
    handler: usize,
    mask: u64,
    restorer: usize,
}

unsafe extern "C" fn sigaction(
    signal: c_int,
    action: *const BionicSigaction,
    previous: *mut BionicSigaction,
) -> c_int {
    // SAFETY: zero is a valid sigaction, and non-null pointers follow the libc contract.
    unsafe {
        let mut wide_action: libc::sigaction = std::mem::zeroed();
        let mut wide_previous: libc::sigaction = std::mem::zeroed();
        if let Some(action) = action.as_ref() {
            wide_action.sa_sigaction = action.handler;
            wide_action.sa_flags = action.flags;
            wide_action.sa_mask = widen(&action.mask);
        }
        let status = libc::sigaction(
            signal,
            if action.is_null() {
                std::ptr::null()
            } else {
                &wide_action
            },
            &mut wide_previous,
        );
        if status == 0
            && let Some(previous) = previous.as_mut()
        {
            *previous = BionicSigaction {
                flags: wide_previous.sa_flags,
                handler: wide_previous.sa_sigaction,
                mask: narrow(&wide_previous.sa_mask),
                restorer: 0,
            };
        }
        status
    }
}

/// Runs a call that changes a mask, translating the new and old sets.
///
/// # Safety
/// Each pointer must be null or point to 8 bytes that `call` may use as a signal set.
unsafe fn with_masks(
    set: *const u64,
    previous: *mut u64,
    call: impl FnOnce(*const libc::sigset_t, *mut libc::sigset_t) -> c_int,
    succeeded: impl FnOnce(c_int) -> bool,
) -> c_int {
    // SAFETY: the caller promises that non-null pointers are valid.
    unsafe {
        let (wide, present) = widen_optional(set);
        let mut wide_previous: libc::sigset_t = std::mem::zeroed();
        let status = call(pointer_if(&wide, present), &mut wide_previous);
        if succeeded(status) && !previous.is_null() {
            previous.write_unaligned(narrow(&wide_previous));
        }
        status
    }
}

unsafe extern "C" fn sigprocmask(how: c_int, set: *const u64, previous: *mut u64) -> c_int {
    // SAFETY: the libc contract makes these null or valid sets.
    unsafe {
        with_masks(
            set,
            previous,
            |new, old| libc::sigprocmask(how, new, old),
            |status| status == 0,
        )
    }
}

unsafe extern "C" fn pthread_sigmask(how: c_int, set: *const u64, previous: *mut u64) -> c_int {
    // SAFETY: the libc contract makes these null or valid sets.
    unsafe {
        with_masks(
            set,
            previous,
            |new, old| libc::pthread_sigmask(how, new, old),
            |status| status == 0,
        )
    }
}

unsafe extern "C" fn sigpending(set: *mut u64) -> c_int {
    // SAFETY: the libc contract makes `set` a valid set.
    unsafe {
        let mut wide: libc::sigset_t = std::mem::zeroed();
        let status = libc::sigpending(&mut wide);
        if status == 0 {
            set.write_unaligned(narrow(&wide));
        }
        status
    }
}

unsafe extern "C" fn sigsuspend(set: *const u64) -> c_int {
    // SAFETY: the libc contract makes `set` a valid set.
    unsafe { libc::sigsuspend(&widen(set)) }
}

unsafe extern "C" fn sigwait(set: *const u64, signal: *mut c_int) -> c_int {
    // SAFETY: the libc contract makes these valid.
    unsafe { libc::sigwait(&widen(set), signal) }
}

unsafe extern "C" fn sigwaitinfo(set: *const u64, info: *mut libc::siginfo_t) -> c_int {
    // SAFETY: the libc contract makes these valid.
    unsafe { libc::sigwaitinfo(&widen(set), info) }
}

unsafe extern "C" fn sigtimedwait(
    set: *const u64,
    info: *mut libc::siginfo_t,
    timeout: *const libc::timespec,
) -> c_int {
    // SAFETY: the libc contract makes these valid.
    unsafe { libc::sigtimedwait(&widen(set), info, timeout) }
}

unsafe extern "C" fn signalfd(fd: c_int, set: *const u64, flags: c_int) -> c_int {
    // SAFETY: the libc contract makes `set` a valid set.
    unsafe { libc::signalfd(fd, &widen(set), flags) }
}

unsafe extern "C" fn ppoll(
    fds: *mut libc::pollfd,
    count: libc::nfds_t,
    timeout: *const libc::timespec,
    set: *const u64,
) -> c_int {
    // SAFETY: the libc contract makes these null or valid.
    unsafe {
        let (wide, present) = widen_optional(set);
        libc::ppoll(fds, count, timeout, pointer_if(&wide, present))
    }
}

unsafe extern "C" fn pselect(
    count: c_int,
    read: *mut libc::fd_set,
    write: *mut libc::fd_set,
    except: *mut libc::fd_set,
    timeout: *const libc::timespec,
    set: *const u64,
) -> c_int {
    // SAFETY: the libc contract makes these null or valid.
    unsafe {
        let (wide, present) = widen_optional(set);
        libc::pselect(
            count,
            read,
            write,
            except,
            timeout,
            pointer_if(&wide, present),
        )
    }
}

unsafe extern "C" fn epoll_pwait(
    fd: c_int,
    events: *mut libc::epoll_event,
    max_events: c_int,
    timeout: c_int,
    set: *const u64,
) -> c_int {
    // SAFETY: the libc contract makes these null or valid.
    unsafe {
        let (wide, present) = widen_optional(set);
        libc::epoll_pwait(fd, events, max_events, timeout, pointer_if(&wide, present))
    }
}

// --- Threads ---

/// glibc's object for a bionic condition variable: the caller's own on x86_64, where both libcs
/// align it alike, and on AArch64 the one `misaligned` keeps for a 4-mod-8 object.
fn host_cond(cond: *mut c_void) -> *mut libc::pthread_cond_t {
    #[cfg(target_arch = "aarch64")]
    return misaligned::host(cond);
    #[cfg(target_arch = "x86_64")]
    return cond.cast();
}

/// glibc's object for a bionic semaphore, as `host_cond` finds a condition variable's.
fn host_sem(sem: *mut c_void) -> *mut libc::sem_t {
    #[cfg(target_arch = "aarch64")]
    return misaligned::host(sem);
    #[cfg(target_arch = "x86_64")]
    return sem.cast();
}

/// bionic's `pthread_cond_t` and `sem_t` are 4-aligned and glibc's 8-aligned on AArch64, where
/// glibc updates their 64-bit sequence words with atomics that fault at an address 4 mod 8, as a
/// condition variable after a mutex in a struct lands. An 8-aligned object is glibc's own, so
/// that path costs one test. A 4-mod-8 one stands for a glibc object on the heap: its own word at
/// offset 4, which is 8-aligned, holds that object's address, set with a compare-and-swap when
/// it is first used, so a zeroed object (`PTHREAD_COND_INITIALIZER`) needs no call and two threads
/// using it at once get the same one. Destroying it frees the glibc object. A process-shared one
/// cannot live on this process's heap, so a misaligned one is refused.
#[cfg(target_arch = "aarch64")]
mod misaligned {
    use std::ffi::{c_int, c_uint, c_void};
    use std::mem::MaybeUninit;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The word that holds a misaligned object's glibc object, or `None` for an 8-aligned object.
    fn slot<T: 'static>(object: *mut c_void) -> Option<&'static AtomicPtr<T>> {
        let address = object as usize;
        if address % 8 != 4 {
            return None;
        }
        // SAFETY: bionic's types are 4-aligned and at least 12 bytes, so the word at offset 4 is
        // inside the caller's object and 8-aligned; only these functions touch the object.
        Some(unsafe { &*((address + 4) as *const AtomicPtr<T>) })
    }

    fn zeroed<T>() -> *mut T {
        Box::into_raw(Box::<T>::new_zeroed()).cast()
    }

    /// The glibc object for `object`, made zeroed on first use.
    pub(super) fn host<T: 'static>(object: *mut c_void) -> *mut T {
        let Some(slot) = slot::<T>(object) else {
            return object.cast();
        };
        let current = slot.load(Ordering::Acquire);
        if !current.is_null() {
            return current;
        }
        let fresh = zeroed::<T>();
        match slot.compare_exchange(
            std::ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => fresh,
            Err(winner) => {
                // SAFETY: `fresh` came from `zeroed` and nobody else saw it.
                drop(unsafe { Box::from_raw(fresh.cast::<MaybeUninit<T>>()) });
                winner
            }
        }
    }

    /// Takes a misaligned object's glibc object out of it, for destroying.
    fn take<T: 'static>(object: *mut c_void) -> Option<*mut T> {
        slot::<T>(object).map(|slot| slot.swap(std::ptr::null_mut(), Ordering::AcqRel))
    }

    /// # Safety
    /// `host` came from `zeroed::<T>` and nothing uses it any more.
    unsafe fn free<T>(host: *mut T) {
        if !host.is_null() {
            // SAFETY: the caller's promise.
            drop(unsafe { Box::from_raw(host.cast::<MaybeUninit<T>>()) });
        }
    }

    unsafe extern "C" fn pthread_cond_init(
        cond: *mut c_void,
        attr: *const libc::pthread_condattr_t,
    ) -> c_int {
        let Some(slot) = slot::<libc::pthread_cond_t>(cond) else {
            // SAFETY: an 8-aligned object is glibc's own, and the C contract makes it writable.
            return unsafe { libc::pthread_cond_init(cond.cast(), attr) };
        };
        let mut shared = libc::PTHREAD_PROCESS_PRIVATE;
        // SAFETY: a non-null attribute is the caller's initialized one.
        if !attr.is_null()
            && unsafe { libc::pthread_condattr_getpshared(attr, &mut shared) } == 0
            && shared != libc::PTHREAD_PROCESS_PRIVATE
        {
            return libc::EINVAL;
        }
        // What the object held may be anything, a freed object's address included, so it is
        // replaced, never read.
        let fresh = zeroed::<libc::pthread_cond_t>();
        // SAFETY: `fresh` is a zeroed glibc object of glibc's size and alignment.
        let status = unsafe { libc::pthread_cond_init(fresh, attr) };
        if status != 0 {
            // SAFETY: nobody saw `fresh`.
            unsafe { free(fresh) };
            return status;
        }
        slot.store(fresh, Ordering::Release);
        0
    }

    unsafe extern "C" fn pthread_cond_destroy(cond: *mut c_void) -> c_int {
        match take::<libc::pthread_cond_t>(cond) {
            None => {
                // SAFETY: an 8-aligned object is glibc's own.
                unsafe { libc::pthread_cond_destroy(cond.cast()) }
            }
            Some(host) if host.is_null() => 0,
            Some(host) => {
                // SAFETY: glibc's destroy returns once no waiter uses the object, which the C
                // contract makes the last use, so it can be freed.
                unsafe {
                    let status = libc::pthread_cond_destroy(host);
                    free(host);
                    status
                }
            }
        }
    }

    unsafe extern "C" fn pthread_cond_signal(cond: *mut c_void) -> c_int {
        // SAFETY: `host` gives glibc an object of its own alignment.
        unsafe { libc::pthread_cond_signal(host(cond)) }
    }

    unsafe extern "C" fn pthread_cond_broadcast(cond: *mut c_void) -> c_int {
        // SAFETY: as above.
        unsafe { libc::pthread_cond_broadcast(host(cond)) }
    }

    unsafe extern "C" fn pthread_cond_wait(
        cond: *mut c_void,
        mutex: *mut libc::pthread_mutex_t,
    ) -> c_int {
        // SAFETY: as above; glibc's mutex atomics are 32-bit, which bionic's alignment allows.
        unsafe { libc::pthread_cond_wait(host(cond), mutex) }
    }

    unsafe extern "C" fn pthread_cond_timedwait(
        cond: *mut c_void,
        mutex: *mut libc::pthread_mutex_t,
        time: *const libc::timespec,
    ) -> c_int {
        // SAFETY: as above.
        unsafe { libc::pthread_cond_timedwait(host(cond), mutex, time) }
    }

    unsafe extern "C" fn sem_init(sem: *mut c_void, shared: c_int, value: c_uint) -> c_int {
        let Some(slot) = slot::<libc::sem_t>(sem) else {
            // SAFETY: an 8-aligned object is glibc's own.
            return unsafe { libc::sem_init(sem.cast(), shared, value) };
        };
        if shared != 0 {
            super::set_errno(libc::ENOSYS);
            return -1;
        }
        let fresh = zeroed::<libc::sem_t>();
        // SAFETY: `fresh` is a zeroed glibc object of glibc's size and alignment.
        if unsafe { libc::sem_init(fresh, 0, value) } != 0 {
            // SAFETY: nobody saw `fresh`.
            unsafe { free(fresh) };
            return -1;
        }
        slot.store(fresh, Ordering::Release);
        0
    }

    unsafe extern "C" fn sem_destroy(sem: *mut c_void) -> c_int {
        match take::<libc::sem_t>(sem) {
            None => {
                // SAFETY: an 8-aligned object is glibc's own.
                unsafe { libc::sem_destroy(sem.cast()) }
            }
            Some(host) => {
                // SAFETY: the C contract makes destroy the last use.
                unsafe {
                    let status = if host.is_null() {
                        0
                    } else {
                        libc::sem_destroy(host)
                    };
                    free(host);
                    status
                }
            }
        }
    }

    unsafe extern "C" fn sem_post(sem: *mut c_void) -> c_int {
        // SAFETY: `host` gives glibc an object of its own alignment.
        unsafe { libc::sem_post(host(sem)) }
    }

    unsafe extern "C" fn sem_wait(sem: *mut c_void) -> c_int {
        // SAFETY: as above.
        unsafe { libc::sem_wait(host(sem)) }
    }

    unsafe extern "C" fn sem_trywait(sem: *mut c_void) -> c_int {
        // SAFETY: as above.
        unsafe { libc::sem_trywait(host(sem)) }
    }

    unsafe extern "C" fn sem_timedwait(sem: *mut c_void, time: *const libc::timespec) -> c_int {
        // SAFETY: as above.
        unsafe { libc::sem_timedwait(host(sem), time) }
    }

    unsafe extern "C" fn sem_getvalue(sem: *mut c_void, value: *mut c_int) -> c_int {
        // SAFETY: as above.
        unsafe { libc::sem_getvalue(host(sem), value) }
    }

    pub const FUNCTIONS: &[(&str, *const ())] = &[
        ("pthread_cond_init", pthread_cond_init as *const ()),
        ("pthread_cond_destroy", pthread_cond_destroy as *const ()),
        ("pthread_cond_signal", pthread_cond_signal as *const ()),
        (
            "pthread_cond_broadcast",
            pthread_cond_broadcast as *const (),
        ),
        ("pthread_cond_wait", pthread_cond_wait as *const ()),
        (
            "pthread_cond_timedwait",
            pthread_cond_timedwait as *const (),
        ),
        ("sem_init", sem_init as *const ()),
        ("sem_destroy", sem_destroy as *const ()),
        ("sem_post", sem_post as *const ()),
        ("sem_wait", sem_wait as *const ()),
        ("sem_trywait", sem_trywait as *const ()),
        ("sem_timedwait", sem_timedwait as *const ()),
        ("sem_getvalue", sem_getvalue as *const ()),
    ];
}

/// On AArch64 glibc's `pthread_attr_t` is 64 bytes and its mutex 48, against bionic's 56 and 40,
/// and three functions clear the whole of glibc's size (`abi/write_extent.c`); every other
/// function on these types stays inside bionic's. These run glibc's on an object of glibc's size
/// and copy back what bionic's holds, which is all glibc ever reads.
#[cfg(target_arch = "aarch64")]
mod wide_objects {
    use std::ffi::{c_int, c_void};
    use std::mem::MaybeUninit;

    const BIONIC_ATTR_SIZE: usize = 56;
    const BIONIC_MUTEX_SIZE: usize = 40;

    /// Runs `call` on a zeroed `T` holding a copy of the caller's `size` bytes, then copies them
    /// back.
    ///
    /// # Safety
    /// `object` must point at `size` writable bytes, `size` at most `size_of::<T>()`.
    unsafe fn through_host<T>(
        object: *mut c_void,
        size: usize,
        call: impl FnOnce(*mut T) -> c_int,
    ) -> c_int {
        let mut host = MaybeUninit::<T>::zeroed();
        // SAFETY: the caller promises `size` bytes at `object`, and `host` holds at least as many.
        unsafe {
            std::ptr::copy_nonoverlapping(object.cast::<u8>(), host.as_mut_ptr().cast(), size);
            let status = call(host.as_mut_ptr());
            std::ptr::copy_nonoverlapping(host.as_ptr().cast::<u8>(), object.cast(), size);
            status
        }
    }

    pub unsafe extern "C" fn pthread_attr_init(attr: *mut c_void) -> c_int {
        // SAFETY: the C contract gives `attr` bionic's size.
        unsafe { through_host(attr, BIONIC_ATTR_SIZE, |host| libc::pthread_attr_init(host)) }
    }

    pub unsafe extern "C" fn pthread_getattr_np(
        thread: libc::pthread_t,
        attr: *mut c_void,
    ) -> c_int {
        // SAFETY: as above.
        unsafe {
            through_host(attr, BIONIC_ATTR_SIZE, |host| {
                libc::pthread_getattr_np(thread, host)
            })
        }
    }

    pub unsafe extern "C" fn pthread_mutex_init(
        mutex: *mut c_void,
        attr: *const libc::pthread_mutexattr_t,
    ) -> c_int {
        // SAFETY: the C contract gives `mutex` bionic's size; bionic's mutex attribute is as
        // large as glibc's (compare.sh).
        unsafe {
            through_host(mutex, BIONIC_MUTEX_SIZE, |host| {
                libc::pthread_mutex_init(host, attr)
            })
        }
    }

    pub const FUNCTIONS: &[(&str, *const ())] = &[
        ("pthread_attr_init", pthread_attr_init as *const ()),
        ("pthread_getattr_np", pthread_getattr_np as *const ()),
        ("pthread_mutex_init", pthread_mutex_init as *const ()),
    ];
}

/// `__pthread_cleanup_t` from bionic's `pthread.h`. glibc keeps no equivalent list.
#[repr(C)]
struct CleanupEntry {
    previous: *mut CleanupEntry,
    routine: Option<unsafe extern "C" fn(*mut c_void)>,
    argument: *mut c_void,
}

thread_local! {
    static CLEANUP_HEAD: Cell<*mut CleanupEntry> = const { Cell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn pthread_cleanup_push(
    entry: *mut CleanupEntry,
    routine: Option<unsafe extern "C" fn(*mut c_void)>,
    argument: *mut c_void,
) {
    CLEANUP_HEAD.with(|head| {
        // SAFETY: the `pthread_cleanup_push` macro passes an entry on the caller's stack.
        unsafe {
            *entry = CleanupEntry {
                previous: head.get(),
                routine,
                argument,
            };
        }
        head.set(entry);
    });
}

unsafe extern "C" fn pthread_cleanup_pop(entry: *mut CleanupEntry, execute: c_int) {
    // SAFETY: the matching push initialized this entry, and it is still on the caller's stack.
    let entry = unsafe { &*entry };
    CLEANUP_HEAD.with(|head| head.set(entry.previous));
    if execute != 0
        && let Some(routine) = entry.routine
    {
        // SAFETY: the routine and argument were registered together.
        unsafe { routine(entry.argument) };
    }
}

// `C-unwind`, because glibc ends the thread with a forced unwind through this frame.
unsafe extern "C-unwind" fn pthread_exit(value: *mut c_void) -> ! {
    unsafe extern "C-unwind" {
        #[link_name = "pthread_exit"]
        fn host_pthread_exit(value: *mut c_void) -> !;
    }
    loop {
        let entry = CLEANUP_HEAD.with(Cell::get);
        if entry.is_null() {
            break;
        }
        // SAFETY: entries stay valid until popped because their frames have not returned.
        unsafe { pthread_cleanup_pop(entry, 1) };
    }
    // SAFETY: forwards the caller's own request to end this thread.
    unsafe { host_pthread_exit(value) }
}

type ForkHandler = Option<unsafe extern "C" fn()>;

// glibc links this one statically into each program, so it cannot be found dynamically.
unsafe extern "C" fn pthread_atfork(
    prepare: ForkHandler,
    parent: ForkHandler,
    child: ForkHandler,
) -> c_int {
    unsafe extern "C" {
        fn __register_atfork(
            prepare: ForkHandler,
            parent: ForkHandler,
            child: ForkHandler,
            dso_handle: *mut c_void,
        ) -> c_int;
    }
    // SAFETY: a null handle means the handlers are never unregistered, and libraries are never
    // unloaded.
    unsafe { __register_atfork(prepare, parent, child, std::ptr::null_mut()) }
}

// --- System configuration ---

extern "C" fn getprogname() -> *const c_char {
    unsafe extern "C" {
        static __progname: *const c_char;
    }
    // SAFETY: glibc sets this before any library can run.
    unsafe { __progname }
}

/// bionic's `_SC_*` values, from `bits/sysconf.h`, mapped to glibc's.
const SYSCONF_NAMES: &[(c_int, c_int)] = &[
    (0x00, libc::_SC_ARG_MAX),
    (0x05, libc::_SC_CHILD_MAX),
    (0x06, libc::_SC_CLK_TCK),
    (0x0A, libc::_SC_NGROUPS_MAX),
    (0x0B, libc::_SC_OPEN_MAX),
    (0x19, libc::_SC_VERSION),
    (0x1B, libc::_SC_STREAM_MAX),
    (0x1C, libc::_SC_TZNAME_MAX),
    (0x26, libc::_SC_IOV_MAX),
    (0x27, libc::_SC_PAGESIZE),
    (0x28, libc::_SC_PAGE_SIZE),
    (0x35, libc::_SC_SEM_NSEMS_MAX),
    (0x36, libc::_SC_SEM_VALUE_MAX),
    (0x46, libc::_SC_TIMERS),
    (0x47, libc::_SC_GETGR_R_SIZE_MAX),
    (0x48, libc::_SC_GETPW_R_SIZE_MAX),
    (0x49, libc::_SC_LOGIN_NAME_MAX),
    (0x4B, libc::_SC_THREAD_KEYS_MAX),
    (0x4C, libc::_SC_THREAD_STACK_MIN),
    (0x4D, libc::_SC_THREAD_THREADS_MAX),
    (0x4E, libc::_SC_TTY_NAME_MAX),
    (0x4F, libc::_SC_THREADS),
    (0x60, libc::_SC_NPROCESSORS_CONF),
    (0x61, libc::_SC_NPROCESSORS_ONLN),
    (0x62, libc::_SC_PHYS_PAGES),
    (0x63, libc::_SC_AVPHYS_PAGES),
    (0x64, libc::_SC_MONOTONIC_CLOCK),
    (0x6D, libc::_SC_CLOCK_SELECTION),
    (0x6F, libc::_SC_HOST_NAME_MAX),
    (0x79, libc::_SC_SYMLOOP_MAX),
];

unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    match SYSCONF_NAMES.iter().find(|(bionic, _)| *bionic == name) {
        // SAFETY: sysconf takes only an integer.
        Some(&(_, glibc)) => unsafe { libc::sysconf(glibc) },
        None => {
            set_errno(libc::EINVAL);
            -1
        }
    }
}

unsafe extern "C" fn nl_langinfo(item: c_int) -> *const c_char {
    const BIONIC_CODESET: c_int = 1;
    if item == BIONIC_CODESET {
        // SAFETY: CODESET is a valid item.
        unsafe { libc::nl_langinfo(libc::CODESET) }
    } else {
        c"".as_ptr()
    }
}

// --- Name resolution ---

/// bionic's `EAI_*` values, mapped to glibc's.
const EAI_CODES: &[(c_int, c_int)] = &[
    (2, libc::EAI_AGAIN),
    (3, libc::EAI_BADFLAGS),
    (4, libc::EAI_FAIL),
    (5, libc::EAI_FAMILY),
    (6, libc::EAI_MEMORY),
    (7, libc::EAI_NODATA),
    (8, libc::EAI_NONAME),
    (9, libc::EAI_SERVICE),
    (10, libc::EAI_SOCKTYPE),
    (11, libc::EAI_SYSTEM),
    (14, libc::EAI_OVERFLOW),
];

/// bionic's `AI_*` flags from `netdb.h`, mapped to glibc's. Lower flags have equal values.
const AI_FLAGS: &[(c_int, c_int)] = &[
    (0x0000_0008, libc::AI_NUMERICSERV),
    (0x0000_0100, libc::AI_ALL),
    (0x0000_0400, libc::AI_ADDRCONFIG),
    (0x0000_0800, libc::AI_V4MAPPED),
];
const AI_SHARED_FLAGS: c_int = libc::AI_PASSIVE | libc::AI_CANONNAME | libc::AI_NUMERICHOST;

/// The two ABIs swap `ai_canonname` and `ai_addr`, so converting either way swaps them.
///
/// # Safety
/// `entry` must be null or the head of a valid list.
unsafe fn swap_addrinfo_fields(mut entry: *mut libc::addrinfo) {
    // SAFETY: the caller promises a valid list, and both fields are pointers.
    unsafe {
        while let Some(current) = entry.as_mut() {
            std::ptr::swap(
                std::ptr::from_mut(&mut current.ai_addr).cast::<usize>(),
                std::ptr::from_mut(&mut current.ai_canonname).cast::<usize>(),
            );
            entry = current.ai_next;
        }
    }
}

unsafe extern "C" fn getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const libc::addrinfo,
    result: *mut *mut libc::addrinfo,
) -> c_int {
    // SAFETY: the libc contract makes these null or valid. A copy of the hints holds only
    // integers that matter: glibc requires their pointer fields to be null anyway.
    unsafe {
        let mut wide_hints: Option<libc::addrinfo> = hints.as_ref().map(|hints| {
            let mut copy = std::ptr::read(hints);
            copy.ai_flags = hints.ai_flags & AI_SHARED_FLAGS;
            for &(bionic, glibc) in AI_FLAGS {
                if hints.ai_flags & bionic != 0 {
                    copy.ai_flags |= glibc;
                }
            }
            copy
        });
        let status = libc::getaddrinfo(
            node,
            service,
            wide_hints
                .as_mut()
                .map_or(std::ptr::null(), |hints| std::ptr::from_ref(hints)),
            result,
        );
        if status == 0 {
            swap_addrinfo_fields(*result);
            return 0;
        }
        bionic_eai(status)
    }
}

unsafe extern "C" fn freeaddrinfo(list: *mut libc::addrinfo) {
    // SAFETY: the list came from `getaddrinfo` above, so swapping restores glibc's layout.
    unsafe {
        swap_addrinfo_fields(list);
        libc::freeaddrinfo(list);
    }
}

unsafe extern "C" fn gai_strerror(code: c_int) -> *const c_char {
    let glibc = EAI_CODES
        .iter()
        .find(|(bionic, _)| *bionic == code)
        .map_or(code, |(_, glibc)| *glibc);
    // SAFETY: gai_strerror accepts any integer.
    unsafe { libc::gai_strerror(glibc) }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Read = unsafe extern "C" fn(*mut c_void, *mut c_char, c_int) -> c_int;
    type Write = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int;
    type Seek = unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64;
    type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
    type Funopen = unsafe extern "C" fn(
        *const c_void,
        Option<Read>,
        Option<Write>,
        Option<Seek>,
        Option<Close>,
    ) -> *mut c_void;

    /// A stream over a byte vector, as a library would write with funopen.
    struct Memory {
        bytes: Vec<u8>,
        position: usize,
        closed: bool,
    }

    unsafe extern "C" fn read(cookie: *mut c_void, buffer: *mut c_char, size: c_int) -> c_int {
        // SAFETY: the test passes a live Memory and the stream gives `size` writable bytes.
        let memory = unsafe { &mut *cookie.cast::<Memory>() };
        let count = (size as usize).min(memory.bytes.len() - memory.position);
        let source = &memory.bytes[memory.position..memory.position + count];
        // SAFETY: as above.
        unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), buffer.cast(), count) };
        memory.position += count;
        count as c_int
    }

    unsafe extern "C" fn write(cookie: *mut c_void, buffer: *const c_char, size: c_int) -> c_int {
        // SAFETY: as in `read`.
        let memory = unsafe { &mut *cookie.cast::<Memory>() };
        // SAFETY: the stream gives `size` readable bytes.
        let source = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
        memory.bytes.truncate(memory.position);
        memory.bytes.extend_from_slice(source);
        memory.position += source.len();
        size
    }

    unsafe extern "C" fn seek(cookie: *mut c_void, offset: i64, whence: c_int) -> i64 {
        // SAFETY: as in `read`.
        let memory = unsafe { &mut *cookie.cast::<Memory>() };
        let base = match whence {
            libc::SEEK_SET => 0,
            libc::SEEK_CUR => memory.position as i64,
            _ => memory.bytes.len() as i64,
        };
        memory.position = (base + offset) as usize;
        memory.position as i64
    }

    unsafe extern "C" fn close(cookie: *mut c_void) -> c_int {
        // SAFETY: as in `read`.
        unsafe { (*cookie.cast::<Memory>()).closed = true };
        7
    }

    fn funopen() -> Funopen {
        let address = resolve(c"funopen", false).expect("funopen resolves");
        // SAFETY: bionic's funopen has this signature.
        unsafe { std::mem::transmute::<usize, Funopen>(address) }
    }

    #[test]
    fn funopen_streams_read_seek_write_and_close_through_the_callbacks() {
        let mut memory = Memory {
            bytes: b"superpack".to_vec(),
            position: 0,
            closed: false,
        };
        let cookie = std::ptr::from_mut(&mut memory).cast::<c_void>();
        // SAFETY: the callbacks match bionic's, and the cookie outlives the stream.
        let file = unsafe { funopen()(cookie, Some(read), Some(write), Some(seek), Some(close)) };
        assert!(!file.is_null());
        let mut text = [0u8; 5];
        // SAFETY: `file` is the stream just opened; the buffer has 5 bytes.
        unsafe {
            assert_eq!(stdio::fread(text.as_mut_ptr().cast(), 1, 5, file), 5);
            assert_eq!(&text, b"super");
            assert_eq!(stdio::fseeko(file, -2, libc::SEEK_END), 0);
            assert_eq!(stdio::ftello(file), 7);
            assert_eq!(stdio::fwrite(b"ce".as_ptr().cast(), 1, 2, file), 2);
            assert_eq!(stdio::fclose(file), 7);
        }
        assert_eq!(memory.bytes, b"superpace");
        assert!(memory.closed);
    }

    #[test]
    fn funopen_without_callbacks_is_einval() {
        // SAFETY: no callback is given, so the cookie is never used.
        let file = unsafe { funopen()(std::ptr::null(), None, None, None, None) };
        assert!(file.is_null());
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn a_read_only_funopen_stream_refuses_writes() {
        let mut memory = Memory {
            bytes: b"abc".to_vec(),
            position: 0,
            closed: false,
        };
        let cookie = std::ptr::from_mut(&mut memory).cast::<c_void>();
        // SAFETY: as above.
        let file = unsafe { funopen()(cookie, Some(read), None, None, None) };
        assert!(!file.is_null());
        // SAFETY: `file` is the stream just opened.
        unsafe {
            assert_eq!(stdio::fwrite(b"x".as_ptr().cast(), 1, 1, file), 0);
            assert_eq!(stdio::fgetc(file), c_int::from(b'a'));
            assert_eq!(stdio::fseeko(file, 0, libc::SEEK_SET), -1);
            assert_eq!(stdio::fclose(file), 0);
        }
        assert_eq!(memory.bytes, b"abc");
    }

    fn function<T>(name: &CStr) -> T {
        let address = resolve(name, false).unwrap_or_else(|| panic!("{name:?} resolves"));
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<usize>());
        // SAFETY: the caller names the bionic signature of `name`.
        unsafe { std::mem::transmute_copy(&address) }
    }

    #[test]
    fn ctype_is_a_pointer_to_bionic_s_table_one_past_eof() {
        let address = resolve(c"_ctype_", true).expect("_ctype_ resolves as data");
        // SAFETY: bionic's `_ctype_` is a `const char*` to 257 entries, the first for EOF.
        let table = unsafe { std::slice::from_raw_parts(*(address as *const *const u8), 257) };
        let class = |c: u8| table[1 + c as usize];
        assert_eq!(table[0], 0);
        assert_eq!(class(b'A'), 0x01 | 0x40);
        assert_eq!(class(b'z'), 0x02);
        assert_eq!(class(b'f'), 0x02 | 0x40);
        assert_eq!(class(b'7'), 0x04);
        assert_eq!(class(b'\t'), 0x08 | 0x20);
        assert_eq!(class(b' '), 0x08 | 0x80);
        assert_eq!(class(b'~'), 0x10);
        assert_eq!(class(0x7f), 0x20);
        assert_eq!(class(0xe9), 0);
    }

    #[test]
    fn write_chk_writes_what_fits_its_object() {
        let write_chk: unsafe extern "C" fn(c_int, *const c_void, usize, usize) -> isize =
            function(c"__write_chk");
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for both ends.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut back = [0u8; 4];
        // SAFETY: both descriptors are the pipe just made, and the buffers are as long as given.
        unsafe {
            assert_eq!(write_chk(fds[1], b"dump".as_ptr().cast(), 4, 4), 4);
            assert_eq!(libc::read(fds[0], back.as_mut_ptr().cast(), 4), 4);
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        assert_eq!(&back, b"dump");
    }

    #[test]
    fn pwrite_chk_writes_what_fits_its_object_at_its_offset() {
        for name in [c"__pwrite_chk", c"__pwrite64_chk"] {
            let pwrite_chk: unsafe extern "C" fn(c_int, *const c_void, usize, i64, usize) -> isize =
                function(name);
            let file = std::env::temp_dir().join(format!("uxdroid-pwrite-{}", std::process::id()));
            let path = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: the path is a C string, and the buffers are as long as given.
            unsafe {
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
                    0o600,
                );
                assert!(fd >= 0);
                assert_eq!(pwrite_chk(fd, b"dump".as_ptr().cast(), 4, 2, 4), 4);
                libc::close(fd);
            }
            assert_eq!(std::fs::read(&file).unwrap(), b"\0\0dump", "{name:?}");
            std::fs::remove_file(&file).unwrap();
        }
    }

    #[test]
    fn umask_chk_sets_a_valid_mask() {
        let umask_chk: unsafe extern "C" fn(libc::mode_t) -> libc::mode_t =
            function(c"__umask_chk");
        // SAFETY: both masks are valid.
        unsafe {
            let old = umask_chk(0o027);
            assert_eq!(umask_chk(old), 0o027);
        }
    }

    #[test]
    fn pthread_gettid_np_names_any_thread() {
        let gettid_np: unsafe extern "C" fn(libc::pthread_t) -> libc::pid_t =
            function(c"pthread_gettid_np");
        // SAFETY: pthread_self is always valid.
        assert_eq!(unsafe { gettid_np(libc::pthread_self()) }, unsafe {
            libc::gettid()
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            // SAFETY: gettid has no preconditions.
            sender
                .send((unsafe { libc::pthread_self() }, unsafe { libc::gettid() }))
                .unwrap();
            wait.recv().unwrap();
        });
        let (handle, tid) = receiver.recv().unwrap();
        // SAFETY: the thread is alive until released.
        assert_eq!(unsafe { gettid_np(handle) }, tid);
        release.send(()).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn system_properties_that_are_never_found_read_as_empty() {
        let read: unsafe extern "C" fn(*const c_void, *mut c_char, *mut c_char) -> c_int =
            function(c"__system_property_read");
        let mut name = [1 as c_char; 32];
        let mut value = [1 as c_char; 92];
        // SAFETY: the buffers have the sizes bionic documents.
        assert_eq!(
            unsafe { read(std::ptr::null(), name.as_mut_ptr(), value.as_mut_ptr()) },
            0
        );
        assert_eq!((name[0], value[0]), (0, 0));
        let _: unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void) =
            function(c"__system_property_read_callback");
    }
    #[test]
    fn functions_with_one_abi_in_both_libcs_bind_to_glibc() {
        // What Instagram 454 calls that glibc defines with bionic's ABI.
        for name in [
            c"aligned_alloc",
            c"alarm",
            c"eventfd_read",
            c"eventfd_write",
            c"execle",
            c"feclearexcept",
            c"fetestexcept",
            c"getpriority",
            c"ldiv",
            c"pthread_getcpuclockid",
            c"readahead",
            c"sysinfo",
            c"tgkill",
            c"timer_create",
            c"timer_delete",
            c"timer_settime",
            c"__libc_current_sigrtmin",
            c"__libc_current_sigrtmax",
        ] {
            assert_eq!(resolve(name, false), host_symbol(name), "{name:?}");
        }
    }

    #[test]
    fn the_64_bit_stream_names_are_the_stream_functions() {
        assert_eq!(resolve(c"fseeko64", false), resolve(c"fseeko", false));
        assert_eq!(resolve(c"ftello64", false), resolve(c"ftello", false));
    }
    #[test]
    fn the_linker_s_own_entry_points_reach_the_loader() {
        // Meta's crash reporter asks libdl.so for these by name.
        for name in [
            c"__loader_android_dlopen_ext",
            c"__loader_dl_iterate_phdr",
            c"__loader_dladdr",
            c"__loader_dlclose",
            c"__loader_dlerror",
            c"__loader_dlopen",
            c"__loader_dlsym",
            c"__loader_dlvsym",
        ] {
            assert!(resolve(name, false).is_some(), "{name:?}");
        }
        let dlsym: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void) -> *mut c_void =
            function(c"__loader_dlsym");
        // SAFETY: RTLD_DEFAULT and a C string; the caller address is only a hint.
        let found = unsafe { dlsym(std::ptr::null_mut(), c"strlen".as_ptr(), std::ptr::null()) };
        assert_eq!(Some(found as usize), resolve(c"strlen", false));
    }
}
