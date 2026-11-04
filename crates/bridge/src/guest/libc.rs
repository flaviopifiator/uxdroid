//! bionic's libc as guest code sees it: a signature for each function the host serves, and the
//! calls that need more than a signature, such as the ones that take a guest callback.
//!
//! Most signatures are generated (`libc_table.py`, into `libc_table.rs`) from the NDK's arm64
//! libraries and headers: every export whose arguments are scalars or point at memory laid out the
//! same for arm64 and x86_64 bionic. The host side is what x86_64 bionic code gets, a shim or an
//! audited glibc function, so that audit covers guest calls too. The rest are written here or left
//! out on purpose; `guest::unsigned_bionic` lists those, and a library importing one still loads
//! and stops naming it if it calls it.

use std::ffi::c_void;

use super::{Cpu, Kind, Type, call, callback};

pub(super) enum Import {
    Function(&'static str),
    Special(fn(&dyn Cpu)),
    /// An object guest code reads in place, such as the legacy `__sF` streams.
    Data,
}

/// Signatures written by hand, ahead of the generated table (`libc_table.py`): functions with no
/// prototype the generator can read, and ones the bridge carries differently.
const FUNCTIONS: &[(&str, &str)] = &[
    // POSIX's, the exported symbol; _GNU_SOURCE renames the declaration to __gnu_strerror_r.
    ("strerror_r", "i32(i32,ptr,u64)"),
    // Declared by no NDK header the generator reads.
    ("strtoll_l", "i64(ptr,ptr,i32,ptr)"),
    ("strtoull_l", "u64(ptr,ptr,i32,ptr)"),
    // FORTIFY's checked functions that clang knows as builtins, so no header declares them.
    ("__memcpy_chk", "ptr(ptr,ptr,u64,u64)"),
    ("__memmove_chk", "ptr(ptr,ptr,u64,u64)"),
    ("__memset_chk", "ptr(ptr,i32,u64,u64)"),
    ("__strcpy_chk", "ptr(ptr,ptr,u64)"),
    ("__stpcpy_chk", "ptr(ptr,ptr,u64)"),
    ("__strcat_chk", "ptr(ptr,ptr,u64)"),
    ("__strncpy_chk", "ptr(ptr,ptr,u64,u64)"),
    ("__stpncpy_chk", "ptr(ptr,ptr,u64,u64)"),
    ("__strncat_chk", "ptr(ptr,ptr,u64,u64)"),
    ("__snprintf_chk", "i32(ptr,u64,i32,u64,ptr,...)"),
    ("__sprintf_chk", "i32(ptr,i32,u64,ptr,...)"),
    // Memory
    ("malloc_usable_size", "u64(ptr)"),
    // Strings
    ("strnlen", "u64(ptr,u64)"),
    // Process, time and files by descriptor
    ("__stack_chk_fail", "void()"),
    ("android_set_abort_message", "void(ptr)"),
    ("getpid", "i32()"),
    ("gettid", "i32()"),
    ("getuid", "u32()"),
    ("time", "i64(ptr)"),
    ("read", "i64(i32,ptr,u64)"),
    ("write", "i64(i32,ptr,u64)"),
    ("lseek", "i64(i32,i64,i32)"),
    // pthread objects that have the same size in both libcs
    ("pthread_self", "u64()"),
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
    "__snprintf_chk",
    "__sprintf_chk",
];

/// Variadic functions whose last fixed argument is a scanf format of bytes. `swscanf`'s format is
/// a wide string, so it keeps passing every remaining integer register.
const SCAN_FUNCTIONS: &[&str] = &["sscanf", "fscanf", "scanf"];

/// Objects guest code reads in place, with the same layout for arm64 and x86_64 bionic; the host
/// gives each only when its audit lists it.
const DATA: &[&str] = &[
    "__sF",
    "environ",
    "stdin",
    "stdout",
    "stderr",
    "optarg",
    "optind",
    "opterr",
    "optopt",
    "__progname",
    "timezone",
    "daylight",
    "tzname",
    "in6addr_any",
    "in6addr_loopback",
    "signgam",
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
    ("__vsnprintf_chk", "__snprintf_chk"),
    ("__vsprintf_chk", "__sprintf_chk"),
    ("vsscanf", "sscanf"),
    ("vfscanf", "fscanf"),
    ("vscanf", "scanf"),
];

pub(super) fn variadic_twin(name: &str) -> Option<&'static str> {
    VARIADIC_TWINS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, twin)| twin)
}

pub(super) type Special = fn(&dyn Cpu);

const SPECIALS: &[(&str, Special)] = &[
    ("pthread_create", pthread_create),
    ("pthread_once", pthread_once),
    ("pthread_key_create", pthread_key_create),
    ("pthread_getattr_np", pthread_getattr_np),
    ("qsort", qsort),
    ("bsearch", bsearch),
    ("atexit", atexit),
    ("__cxa_atexit", cxa_atexit),
    ("dl_iterate_phdr", dl_iterate_phdr),
    ("getauxval", getauxval),
    ("syscall", syscall),
];

pub(super) fn find(name: &str) -> Option<Import> {
    if DATA.contains(&name) {
        return Some(Import::Data);
    }
    let specials = SPECIALS.iter().chain(super::convert::SPECIALS);
    if let Some(&(_, special)) = specials.clone().find(|(known, _)| *known == name) {
        return Some(Import::Special(special));
    }
    if let Some(&(_, signature)) = FUNCTIONS.iter().find(|(known, _)| *known == name) {
        return Some(Import::Function(signature));
    }
    let generated = super::libc_table::SIGNED;
    generated
        .binary_search_by(|(known, _)| (*known).cmp(name))
        .ok()
        .map(|index| Import::Function(generated[index].1))
}

pub(super) fn is_format_function(name: &str) -> bool {
    FORMAT_FUNCTIONS.contains(&name)
}

pub(super) fn is_scan_function(name: &str) -> bool {
    SCAN_FUNCTIONS.contains(&name)
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

/// The host's attributes of a thread, but for the calling thread the stack is the guest stack its
/// code runs on, not the host's: runtimes such as Dart's find their stack limit this way. Another
/// thread's attributes keep its host stack.
fn pthread_getattr_np(cpu: &dyn Cpu) {
    let thread = cpu.x(0) as libc::pthread_t;
    let attributes = cpu.x(1) as usize as *mut libc::pthread_attr_t;
    // SAFETY: `pthread_attr_t` has the same size in both libcs, and the guest passes a writable one
    // with a thread ID that pthread_self or pthread_create gave it.
    let mut status = unsafe { libc::pthread_getattr_np(thread, attributes) };
    // SAFETY: pthread_self has no preconditions.
    if status == 0
        && thread == unsafe { libc::pthread_self() }
        && let Some((bottom, top)) = super::current_stack()
    {
        // SAFETY: the attributes were just initialized by pthread_getattr_np.
        status = unsafe {
            libc::pthread_attr_setstack(
                attributes,
                bottom as usize as *mut c_void,
                (top - bottom) as usize,
            )
        };
    }
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
    let value = match cpu.x(0) {
        AT_HWCAP => super::GUEST_HWCAP,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Signature;

    #[test]
    fn only_byte_formats_place_scanf_pointers() {
        assert!(is_scan_function("sscanf"));
        assert!(!is_scan_function("swscanf"));
    }

    #[test]
    fn functions_whose_arguments_differ_on_arm64_never_take_the_generic_path() {
        for name in [
            "stat",
            "fstat",
            "lstat",
            "fstatat",
            "fstat64",
            "epoll_ctl",
            "epoll_wait",
            "sigaction",
            "setjmp",
            "longjmp",
            "sigsetjmp",
            "open",
            "openat",
            "fcntl",
            "pipe2",
            "fesetround",
            "dlsym",
            "dlopen",
            "strtold",
            "ioctl",
            "syscall",
            "tcgetattr",
        ] {
            assert!(
                !matches!(find(name), Some(Import::Function(_))),
                "{name} must not bind by signature alone"
            );
        }
    }

    #[test]
    fn bionic_functions_with_matching_layouts_are_signed() {
        for (name, expected) in [
            ("socket", "i32(i32,i32,i32)"),
            ("readdir", "ptr(ptr)"),
            ("poll", "i32(ptr,u32,i32)"),
            ("sigemptyset", "i32(ptr)"),
            ("getaddrinfo", "i32(ptr,ptr,ptr,ptr)"),
            ("strerror_r", "i32(i32,ptr,u64)"),
            ("sscanf", "i32(ptr,ptr,...)"),
            ("acosf", "f32(f32)"),
        ] {
            match find(name) {
                Some(Import::Function(signature)) => assert_eq!(signature, expected, "{name}"),
                _ => panic!("{name} has no guest signature"),
            }
        }
    }

    #[test]
    fn every_listed_signature_parses() {
        let generated = super::super::libc_table::SIGNED.iter();
        for (name, signature) in FUNCTIONS.iter().chain(generated) {
            assert!(
                matches!(Signature::parse(signature), Ok(Signature::Function { .. })),
                "{name}: {signature}"
            );
        }
    }
}
