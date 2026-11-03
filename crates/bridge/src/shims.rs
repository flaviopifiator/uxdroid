//! Bionic symbols that glibc lacks or defines with a different ABI.
//!
//! Struct sizes and constants below were measured by compiling one probe against both libcs:
//! `sigset_t` is 8 bytes in bionic and 128 in glibc, `jmp_buf` 88 and 200, `fpos_t` 8 and 16.
//! `long double` has the same size with another format: binary128 and x87 80-bit.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_int, c_long, c_void};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use crate::loader;

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
    let text = name.to_str().ok()?;
    match table().get(text) {
        Some(Target::Function(address)) => Some(*address as usize),
        Some(Target::Alias(target)) => host_symbol(target),
        None => {
            let address = host_symbol(name)?;
            if audited().contains(text) {
                Some(address)
            } else if is_data {
                None
            } else {
                unaudited_trap(text)
            }
        }
    }
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
    let _ = std::io::stderr().write_fmt(message);
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
        let mut table = HashMap::new();
        for &(name, address) in TRAPS
            .iter()
            .chain(STREAM_FUNCTIONS)
            .chain(crate::paths::PATH_FUNCTIONS)
        {
            table.insert(name, Target::Function(address));
        }
        table.insert(
            "__sF",
            Target::Function(std::ptr::from_ref(&LEGACY_STREAMS).cast()),
        );
        for &(name, target) in ALIASES {
            table.insert(name, Target::Alias(target));
        }
        for &(name, address) in FUNCTIONS {
            table.insert(name, Target::Function(address.0));
        }
        table
    })
}

const ALIASES: &[(&str, &CStr)] = &[
    ("__errno", c"__errno_location"),
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
    "__system_property_find" => system_property_find,
    "__system_property_get" => system_property_get,
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
    "fpathconf" => fpathconf,
    "fprintf" => uxdroid_fprintf,
    "freeaddrinfo" => freeaddrinfo,
    "fscanf" => uxdroid_fscanf,
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
    "printf" => uxdroid_printf,
    "pselect" => pselect,
    "pthread_atfork" => pthread_atfork,
    "pthread_exit" => pthread_exit,
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

variadic_entry!("uxdroid_fprintf", vfprintf, "16", "rdx");
variadic_entry!("uxdroid_fscanf", vfscanf, "16", "rdx");
variadic_entry!("uxdroid_fwprintf", vfwprintf, "16", "rdx");
variadic_entry!("uxdroid_fwscanf", vfwscanf, "16", "rdx");
variadic_entry!("uxdroid_printf", vprintf, "8", "rsi");
variadic_entry!("uxdroid_sprintf", vsprintf, "16", "rdx");
variadic_entry!("uxdroid_snprintf", checked_vsnprintf, "24", "rcx");
variadic_entry!("uxdroid_asprintf", vasprintf, "16", "rdx");
variadic_entry!("uxdroid_dprintf", vdprintf, "16", "rdx");
variadic_entry!("uxdroid_sscanf", vsscanf, "16", "rdx");
variadic_entry!("uxdroid_swprintf", vswprintf, "24", "rcx");
variadic_entry!("uxdroid_syslog", vsyslog, "16", "rdx");
variadic_entry!("uxdroid_sprintf_chk", vsprintf_chk, "32", "r8");
variadic_entry!("uxdroid_snprintf_chk", vsnprintf_chk, "40", "r9");

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

const JUMP_MASK_OFFSET: usize = 72;

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

// --- Fortify ---

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
