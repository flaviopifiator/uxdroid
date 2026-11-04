//! bionic's libc as guest code sees it: a signature for each function the host serves, and the
//! calls that need more than a signature, such as the ones that take a guest callback.
//!
//! Only functions whose arguments have the same meaning for arm64 bionic and the host are listed:
//! scalars, and pointers to memory whose layout matches (strings, `timespec`, `pthread_*_t` of the
//! same size, opaque `FILE`). Functions taking structs that differ on arm64 (`stat`, `sigaction`,
//! `sigset_t`, `jmp_buf`, `ucontext_t`), `va_list` or open flags with other values are missing on
//! purpose; a library importing one still loads and stops naming it if it calls it.

use std::ffi::c_void;

use super::{Cpu, Kind, Type, call, callback};

pub(super) enum Import {
    Function(&'static str),
    Special(fn(&dyn Cpu)),
    /// An object guest code reads in place, such as the legacy `__sF` streams.
    Data,
}

const FUNCTIONS: &[(&str, &str)] = &[
    // Memory
    ("malloc", "ptr(u64)"),
    ("calloc", "ptr(u64,u64)"),
    ("realloc", "ptr(ptr,u64)"),
    ("free", "void(ptr)"),
    ("posix_memalign", "i32(ptr,u64,u64)"),
    ("memalign", "ptr(u64,u64)"),
    ("aligned_alloc", "ptr(u64,u64)"),
    ("malloc_usable_size", "u64(ptr)"),
    ("memcpy", "ptr(ptr,ptr,u64)"),
    ("memmove", "ptr(ptr,ptr,u64)"),
    ("memset", "ptr(ptr,i32,u64)"),
    ("memcmp", "i32(ptr,ptr,u64)"),
    ("memchr", "ptr(ptr,i32,u64)"),
    ("memrchr", "ptr(ptr,i32,u64)"),
    // Strings
    ("strlen", "u64(ptr)"),
    ("strnlen", "u64(ptr,u64)"),
    ("strcmp", "i32(ptr,ptr)"),
    ("strncmp", "i32(ptr,ptr,u64)"),
    ("strcasecmp", "i32(ptr,ptr)"),
    ("strncasecmp", "i32(ptr,ptr,u64)"),
    ("strcpy", "ptr(ptr,ptr)"),
    ("strncpy", "ptr(ptr,ptr,u64)"),
    ("strcat", "ptr(ptr,ptr)"),
    ("strncat", "ptr(ptr,ptr,u64)"),
    ("strlcpy", "u64(ptr,ptr,u64)"),
    ("strlcat", "u64(ptr,ptr,u64)"),
    ("strchr", "ptr(ptr,i32)"),
    ("strrchr", "ptr(ptr,i32)"),
    ("strstr", "ptr(ptr,ptr)"),
    ("strdup", "ptr(ptr)"),
    ("strndup", "ptr(ptr,u64)"),
    ("strerror", "ptr(i32)"),
    ("strtol", "i64(ptr,ptr,i32)"),
    ("strtoul", "u64(ptr,ptr,i32)"),
    ("strtoll", "i64(ptr,ptr,i32)"),
    ("strtoull", "u64(ptr,ptr,i32)"),
    ("strtod", "f64(ptr,ptr)"),
    ("strtof", "f32(ptr,ptr)"),
    ("atoi", "i32(ptr)"),
    ("atol", "i64(ptr)"),
    // Process, time and files by descriptor
    ("__errno", "ptr()"),
    ("abort", "void()"),
    ("exit", "void(i32)"),
    ("_exit", "void(i32)"),
    ("__stack_chk_fail", "void()"),
    ("android_set_abort_message", "void(ptr)"),
    ("__system_property_get", "i32(ptr,ptr)"),
    ("getpid", "i32()"),
    ("gettid", "i32()"),
    ("getuid", "u32()"),
    ("sleep", "u32(u32)"),
    ("usleep", "i32(u32)"),
    ("nanosleep", "i32(ptr,ptr)"),
    ("clock_gettime", "i32(i32,ptr)"),
    ("gettimeofday", "i32(ptr,ptr)"),
    ("time", "i64(ptr)"),
    ("sched_yield", "i32()"),
    ("close", "i32(i32)"),
    ("read", "i64(i32,ptr,u64)"),
    ("write", "i64(i32,ptr,u64)"),
    ("lseek", "i64(i32,i64,i32)"),
    // stdio
    ("fflush", "i32(ptr)"),
    ("fputc", "i32(i32,ptr)"),
    ("fputs", "i32(ptr,ptr)"),
    ("fwrite", "u64(ptr,u64,u64,ptr)"),
    ("fread", "u64(ptr,u64,u64,ptr)"),
    ("puts", "i32(ptr)"),
    ("printf", "i32(ptr,...)"),
    ("fprintf", "i32(ptr,ptr,...)"),
    ("sprintf", "i32(ptr,ptr,...)"),
    ("snprintf", "i32(ptr,u64,ptr,...)"),
    ("dprintf", "i32(i32,ptr,...)"),
    ("asprintf", "i32(ptr,ptr,...)"),
    ("openlog", "void(ptr,i32,i32)"),
    ("closelog", "void()"),
    ("syslog", "void(i32,ptr,...)"),
    // pthread objects that have the same size in both libcs
    ("pthread_join", "i32(u64,ptr)"),
    ("pthread_detach", "i32(u64)"),
    ("pthread_self", "u64()"),
    ("pthread_equal", "i32(u64,u64)"),
    ("pthread_attr_init", "i32(ptr)"),
    ("pthread_attr_destroy", "i32(ptr)"),
    ("pthread_attr_setstacksize", "i32(ptr,u64)"),
    ("pthread_attr_setdetachstate", "i32(ptr,i32)"),
    ("pthread_mutex_init", "i32(ptr,ptr)"),
    ("pthread_mutex_destroy", "i32(ptr)"),
    ("pthread_mutex_lock", "i32(ptr)"),
    ("pthread_mutex_trylock", "i32(ptr)"),
    ("pthread_mutex_unlock", "i32(ptr)"),
    ("pthread_mutexattr_init", "i32(ptr)"),
    ("pthread_mutexattr_destroy", "i32(ptr)"),
    ("pthread_mutexattr_settype", "i32(ptr,i32)"),
    ("pthread_cond_init", "i32(ptr,ptr)"),
    ("pthread_cond_destroy", "i32(ptr)"),
    ("pthread_cond_wait", "i32(ptr,ptr)"),
    ("pthread_cond_timedwait", "i32(ptr,ptr,ptr)"),
    ("pthread_cond_signal", "i32(ptr)"),
    ("pthread_cond_broadcast", "i32(ptr)"),
    ("pthread_rwlock_init", "i32(ptr,ptr)"),
    ("pthread_rwlock_destroy", "i32(ptr)"),
    ("pthread_rwlock_rdlock", "i32(ptr)"),
    ("pthread_rwlock_wrlock", "i32(ptr)"),
    ("pthread_rwlock_unlock", "i32(ptr)"),
    ("pthread_getspecific", "ptr(u32)"),
    ("pthread_setspecific", "i32(u32,ptr)"),
    ("pthread_key_delete", "i32(u32)"),
    // libm
    ("sqrt", "f64(f64)"),
    ("sin", "f64(f64)"),
    ("cos", "f64(f64)"),
    ("tan", "f64(f64)"),
    ("atan2", "f64(f64,f64)"),
    ("exp", "f64(f64)"),
    ("log", "f64(f64)"),
    ("log10", "f64(f64)"),
    ("pow", "f64(f64,f64)"),
    ("floor", "f64(f64)"),
    ("ceil", "f64(f64)"),
    ("fabs", "f64(f64)"),
    ("fmod", "f64(f64,f64)"),
    ("sqrtf", "f32(f32)"),
    ("sinf", "f32(f32)"),
    ("cosf", "f32(f32)"),
    ("expf", "f32(f32)"),
    ("logf", "f32(f32)"),
    ("powf", "f32(f32,f32)"),
    ("floorf", "f32(f32)"),
    ("ceilf", "f32(f32)"),
    // Runs every library's destructors on exit; nothing guest-specific.
    ("__cxa_finalize", "void(ptr)"),
];

/// Variadic functions whose last fixed argument is a printf format.
const FORMAT_FUNCTIONS: &[&str] = &[
    "__android_log_print",
    "__android_log_buf_print",
    "__android_log_assert",
    "printf",
    "fprintf",
    "sprintf",
    "snprintf",
    "dprintf",
    "asprintf",
    "syslog",
];

/// `v` functions of the printf family and the variadic function that serves each: the guest's
/// `va_list` is read by the format and the twin gets the arguments as if called directly.
const VARIADIC_TWINS: &[(&str, &str)] = &[
    ("vprintf", "printf"),
    ("vfprintf", "fprintf"),
    ("vsprintf", "sprintf"),
    ("vsnprintf", "snprintf"),
    ("vdprintf", "dprintf"),
    ("vasprintf", "asprintf"),
    ("vsyslog", "syslog"),
    ("__android_log_vprint", "__android_log_print"),
];

pub(super) fn variadic_twin(name: &str) -> Option<&'static str> {
    VARIADIC_TWINS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, twin)| twin)
}

type Special = fn(&dyn Cpu);

const SPECIALS: &[(&str, Special)] = &[
    ("pthread_create", pthread_create),
    ("pthread_once", pthread_once),
    ("pthread_key_create", pthread_key_create),
    ("qsort", qsort),
    ("bsearch", bsearch),
    ("atexit", atexit),
    ("__cxa_atexit", cxa_atexit),
    ("dl_iterate_phdr", dl_iterate_phdr),
    ("getauxval", getauxval),
    ("syscall", syscall),
];

pub(super) fn find(name: &str) -> Option<Import> {
    if name == "__sF" {
        return Some(Import::Data);
    }
    if let Some(&(_, special)) = SPECIALS.iter().find(|(known, _)| *known == name) {
        return Some(Import::Special(special));
    }
    FUNCTIONS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, signature)| Import::Function(signature))
}

pub(super) fn is_format_function(name: &str) -> bool {
    FORMAT_FUNCTIONS.contains(&name)
}

/// A host function pointer for a guest callback, or the program stops: a callback that cannot
/// be made would otherwise be called through a null pointer later.
fn host_callback(cpu: &dyn Cpu, target: u64, signature: &str) -> usize {
    match callback(target, signature, Kind::Plain) {
        Ok(thunk) => thunk,
        Err(reason) => super::fail(cpu, format_args!("{reason}")),
    }
}

fn set_i32(cpu: &dyn Cpu, value: i32) {
    cpu.set_x(0, value as i64 as u64);
}

struct Start {
    routine: u64,
    argument: u64,
}

extern "C" fn thread_main(start: *mut c_void) -> *mut c_void {
    // SAFETY: `pthread_create` below passes a box it leaked for this thread.
    let start = unsafe { Box::from_raw(start.cast::<Start>()) };
    match call(start.routine, &[Type::Ptr], &[start.argument]) {
        Ok((result, _)) => result as usize as *mut c_void,
        Err(reason) => crate::shims::die(format_args!("uxdroid: guest thread stopped: {reason}\n")),
    }
}

fn pthread_create(cpu: &dyn Cpu) {
    let start = Box::into_raw(Box::new(Start {
        routine: cpu.x(2),
        argument: cpu.x(3),
    }));
    // SAFETY: the guest passes a writable `pthread_t` and an attribute object made by the host's
    // own pthread_attr functions, of the same size; `thread_main` takes ownership of `start`.
    let status = unsafe {
        libc::pthread_create(
            cpu.x(0) as usize as *mut libc::pthread_t,
            cpu.x(1) as usize as *const libc::pthread_attr_t,
            thread_main,
            start.cast(),
        )
    };
    if status != 0 {
        // SAFETY: the thread was not created, so the box is still ours.
        drop(unsafe { Box::from_raw(start) });
    }
    set_i32(cpu, status);
}

fn pthread_once(cpu: &dyn Cpu) {
    let init = host_callback(cpu, cpu.x(1), "void()");
    // SAFETY: the guest passes its `pthread_once_t`, an int in both libcs; the thunk has the
    // required signature.
    let status = unsafe {
        libc::pthread_once(
            cpu.x(0) as usize as *mut libc::pthread_once_t,
            std::mem::transmute::<usize, extern "C" fn()>(init),
        )
    };
    set_i32(cpu, status);
}

fn pthread_key_create(cpu: &dyn Cpu) {
    let destructor = match cpu.x(1) {
        0 => None,
        target => {
            let thunk = host_callback(cpu, target, "void(ptr)");
            // SAFETY: the thunk has the destructor's signature.
            Some(unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(thunk) })
        }
    };
    // SAFETY: `pthread_key_t` is 4 bytes in both libcs, and the guest passes a writable one.
    let status = unsafe {
        libc::pthread_key_create(cpu.x(0) as usize as *mut libc::pthread_key_t, destructor)
    };
    set_i32(cpu, status);
}

type Comparator = unsafe extern "C" fn(*const c_void, *const c_void) -> i32;

fn comparator(cpu: &dyn Cpu, target: u64) -> Comparator {
    let thunk = host_callback(cpu, target, "i32(ptr,ptr)");
    // SAFETY: the thunk has the comparator's signature.
    unsafe { std::mem::transmute::<usize, Comparator>(thunk) }
}

fn qsort(cpu: &dyn Cpu) {
    let compare = comparator(cpu, cpu.x(3));
    // SAFETY: the guest passes its array, count and element size, as qsort's contract requires.
    unsafe {
        libc::qsort(
            cpu.x(0) as usize as *mut c_void,
            cpu.x(1) as usize,
            cpu.x(2) as usize,
            Some(compare),
        );
    }
}

fn bsearch(cpu: &dyn Cpu) {
    let compare = comparator(cpu, cpu.x(4));
    // SAFETY: as for qsort.
    let found = unsafe {
        libc::bsearch(
            cpu.x(0) as usize as *const c_void,
            cpu.x(1) as usize as *const c_void,
            cpu.x(2) as usize,
            cpu.x(3) as usize,
            Some(compare),
        )
    };
    cpu.set_x(0, found as u64);
}

fn atexit(cpu: &dyn Cpu) {
    let function = host_callback(cpu, cpu.x(0), "void()");
    // SAFETY: the thunk has the handler's signature and lives as long as the process.
    let status = unsafe { libc::atexit(std::mem::transmute::<usize, extern "C" fn()>(function)) };
    set_i32(cpu, status);
}

fn cxa_atexit(cpu: &dyn Cpu) {
    unsafe extern "C" {
        fn __cxa_atexit(
            function: unsafe extern "C" fn(*mut c_void),
            argument: *mut c_void,
            dso: *mut c_void,
        ) -> i32;
    }
    let function = host_callback(cpu, cpu.x(0), "void(ptr)");
    // SAFETY: the thunk has the handler's signature; the argument and handle pass through.
    let status = unsafe {
        __cxa_atexit(
            std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(function),
            cpu.x(1) as usize as *mut c_void,
            cpu.x(2) as usize as *mut c_void,
        )
    };
    set_i32(cpu, status);
}

fn dl_iterate_phdr(cpu: &dyn Cpu) {
    type Visit = unsafe extern "C" fn(*mut libc::dl_phdr_info, usize, *mut c_void) -> i32;
    let visit = host_callback(cpu, cpu.x(0), "i32(ptr,u64,ptr)");
    // SAFETY: `dl_phdr_info` has the same layout for arm64 bionic and the host, as both are
    // LP64 ELF; the thunk has the callback's signature.
    let status = unsafe {
        crate::loader::dl_iterate_phdr(
            Some(std::mem::transmute::<usize, Visit>(visit)),
            cpu.x(1) as usize as *mut c_void,
        )
    };
    set_i32(cpu, status);
}

/// What arm64 code learns about its CPU: ARMv8.0 with floating point and Advanced SIMD, the
/// features every engine implements, never the host's x86 feature bits.
fn getauxval(cpu: &dyn Cpu) {
    const AT_HWCAP: u64 = 16;
    const AT_HWCAP2: u64 = 26;
    const HWCAP_FP: u64 = 1 << 0;
    const HWCAP_ASIMD: u64 = 1 << 1;
    let value = match cpu.x(0) {
        AT_HWCAP => HWCAP_FP | HWCAP_ASIMD,
        AT_HWCAP2 => 0,
        // SAFETY: getauxval takes any type and returns 0 for unknown ones.
        other => unsafe { libc::getauxval(other) },
    };
    cpu.set_x(0, value);
}

/// arm64 system call numbers that have the same arguments on x86_64, with the host's number.
const SYSCALLS: &[(u64, i64)] = &[
    (98, libc::SYS_futex),
    (113, libc::SYS_clock_gettime),
    (124, libc::SYS_sched_yield),
    (172, libc::SYS_getpid),
    (178, libc::SYS_gettid),
    (278, libc::SYS_getrandom),
];

/// The host's result of the guest's system call, or `None` for an arm64 number with no mapping.
fn host_syscall(number: u64, arguments: [u64; 6]) -> Option<i64> {
    let &(_, host) = SYSCALLS.iter().find(|(guest, _)| *guest == number)?;
    // SAFETY: each listed call takes integers and pointers with the same meaning on both
    // architectures, passed through as the guest gave them.
    Some(unsafe {
        libc::syscall(
            host,
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
            arguments[5],
        )
    })
}

/// bionic's `syscall(number, ...)`: -1 and errno on failure.
fn syscall(cpu: &dyn Cpu) {
    let arguments = [cpu.x(1), cpu.x(2), cpu.x(3), cpu.x(4), cpu.x(5), cpu.x(6)];
    match host_syscall(cpu.x(0), arguments) {
        Some(result) => cpu.set_x(0, result as u64),
        None => {
            // SAFETY: errno is this thread's.
            unsafe { *libc::__errno_location() = libc::ENOSYS };
            cpu.set_x(0, u64::MAX);
        }
    }
}

/// `svc #0`: the kernel's convention, number in x8, arguments in x0 to x5, and -errno in x0 on
/// failure.
pub(super) fn raw_syscall(cpu: &dyn Cpu) {
    let arguments = [cpu.x(0), cpu.x(1), cpu.x(2), cpu.x(3), cpu.x(4), cpu.x(5)];
    let result = match host_syscall(cpu.x(8), arguments) {
        // SAFETY: errno is this thread's, set by the call just made.
        Some(-1) => -i64::from(unsafe { *libc::__errno_location() }),
        Some(result) => result,
        None => -i64::from(libc::ENOSYS),
    };
    cpu.set_x(0, result as u64);
}
