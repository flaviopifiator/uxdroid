//! bionic's data symbols that glibc lacks, or has with another layout, as objects of bionic's
//! layout and values. Data cannot stop the program when read, as a missing function's stub does,
//! so each is implemented. The values were read from the NDK's own bionic (`libc.a` and `libm.a`)
//! by a static program printing them, for x86_64 and arm64.
//!
//! Objects that only bionic functions read, such as `optreset` and the `error_*` counters, are
//! bionic's variables with nothing behind them yet: glibc's `getopt` and `error`, which apps
//! reach today, keep state of their own, and their bionic forms are not audited.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicUsize, Ordering};

/// bionic's `NSIG` on LP64: 64 signals and the unused 0.
const NSIG: usize = 65;

/// bionic's `__BIONIC_SIGDEF` list: each signal's BSD name, without `SIG`, and its description.
/// The real-time signals and 0 have neither.
const SIGNALS: &[(usize, &CStr, &CStr)] = &[
    (1, c"HUP", c"Hangup"),
    (2, c"INT", c"Interrupt"),
    (3, c"QUIT", c"Quit"),
    (4, c"ILL", c"Illegal instruction"),
    (5, c"TRAP", c"Trap"),
    (6, c"ABRT", c"Aborted"),
    (7, c"BUS", c"Bus error"),
    (8, c"FPE", c"Floating point exception"),
    (9, c"KILL", c"Killed"),
    (10, c"USR1", c"User signal 1"),
    (11, c"SEGV", c"Segmentation fault"),
    (12, c"USR2", c"User signal 2"),
    (13, c"PIPE", c"Broken pipe"),
    (14, c"ALRM", c"Alarm clock"),
    (15, c"TERM", c"Terminated"),
    (16, c"STKFLT", c"Stack fault"),
    (17, c"CHLD", c"Child exited"),
    (18, c"CONT", c"Continue"),
    (19, c"STOP", c"Stopped (signal)"),
    (20, c"TSTP", c"Stopped"),
    (21, c"TTIN", c"Stopped (tty input)"),
    (22, c"TTOU", c"Stopped (tty output)"),
    (23, c"URG", c"Urgent I/O condition"),
    (24, c"XCPU", c"CPU time limit exceeded"),
    (25, c"XFSZ", c"File size limit exceeded"),
    (26, c"VTALRM", c"Virtual timer expired"),
    (27, c"PROF", c"Profiling timer expired"),
    (28, c"WINCH", c"Window size changed"),
    (29, c"IO", c"I/O possible"),
    (30, c"PWR", c"Power failure"),
    (31, c"SYS", c"Bad system call"),
];

/// An array of C string pointers, as `const char* const sys_signame[NSIG]` is.
#[repr(transparent)]
struct Strings<const N: usize>([*const c_char; N]);
// SAFETY: every pointer is null or points at an immutable static string.
unsafe impl<const N: usize> Sync for Strings<N> {}

const fn signal_strings(names: bool) -> [*const c_char; NSIG] {
    let mut table = [std::ptr::null(); NSIG];
    let mut index = 0;
    while index < SIGNALS.len() {
        let (signal, name, description) = SIGNALS[index];
        table[signal] = if names {
            name.as_ptr()
        } else {
            description.as_ptr()
        };
        index += 1;
    }
    table
}

static SYS_SIGNAME: Strings<NSIG> = Strings(signal_strings(true));
static SYS_SIGLIST: Strings<NSIG> = Strings(signal_strings(false));

/// bionic's `struct res_sym`, 24 bytes on LP64.
#[repr(C)]
struct ResSym {
    number: c_int,
    name: *const c_char,
    human_name: *const c_char,
}

type Row = (c_int, Option<&'static CStr>, Option<&'static CStr>);

const fn text(text: Option<&'static CStr>) -> *const c_char {
    match text {
        Some(text) => text.as_ptr(),
        None => std::ptr::null(),
    }
}

const fn res_syms<const N: usize>(rows: [Row; N]) -> [ResSym; N] {
    let mut table = [const {
        ResSym {
            number: 0,
            name: std::ptr::null(),
            human_name: std::ptr::null(),
        }
    }; N];
    let mut index = 0;
    while index < N {
        let (number, name, human_name) = rows[index];
        table[index] = ResSym {
            number,
            name: text(name),
            human_name: text(human_name),
        };
        index += 1;
    }
    table
}

#[repr(transparent)]
struct ResSyms<const N: usize>([ResSym; N]);
// SAFETY: every pointer is null or points at an immutable static string.
unsafe impl<const N: usize> Sync for ResSyms<N> {}

/// bionic's `__p_class_syms`, from BSD's `res_debug.c`: the last entry, `C_IN` with no name, ends it.
static P_CLASS_SYMS: ResSyms<8> = ResSyms(res_syms([
    (1, Some(c"IN"), None),
    (3, Some(c"CH"), None),
    (3, Some(c"CHAOS"), None),
    (4, Some(c"HS"), None),
    (4, Some(c"HESIOD"), None),
    (255, Some(c"ANY"), None),
    (254, Some(c"NONE"), None),
    (1, None, None),
]));

/// bionic's `__p_type_syms`, from BSD's `res_debug.c`, its spellings and repeats included; an entry
/// with no name ends it.
static P_TYPE_SYMS: ResSyms<69> = ResSyms(res_syms([
    (1, Some(c"A"), Some(c"address")),
    (2, Some(c"NS"), Some(c"name server")),
    (3, Some(c"MD"), Some(c"mail destination (deprecated)")),
    (4, Some(c"MF"), Some(c"mail forwarder (deprecated)")),
    (5, Some(c"CNAME"), Some(c"canonical name")),
    (6, Some(c"SOA"), Some(c"start of authority")),
    (7, Some(c"MB"), Some(c"mailbox")),
    (8, Some(c"MG"), Some(c"mail group member")),
    (9, Some(c"MR"), Some(c"mail rename")),
    (10, Some(c"NULL"), Some(c"null")),
    (11, Some(c"WKS"), Some(c"well-known service (deprecated)")),
    (12, Some(c"PTR"), Some(c"domain name pointer")),
    (13, Some(c"HINFO"), Some(c"host information")),
    (14, Some(c"MINFO"), Some(c"mailbox information")),
    (15, Some(c"MX"), Some(c"mail exchanger")),
    (16, Some(c"TXT"), Some(c"text")),
    (17, Some(c"RP"), Some(c"responsible person")),
    (18, Some(c"AFSDB"), Some(c"DCE or AFS server")),
    (19, Some(c"X25"), Some(c"X25 address")),
    (20, Some(c"ISDN"), Some(c"ISDN address")),
    (21, Some(c"RT"), Some(c"router")),
    (22, Some(c"NSAP"), Some(c"nsap address")),
    (23, Some(c"NSAP_PTR"), Some(c"domain name pointer")),
    (24, Some(c"SIG"), Some(c"signature")),
    (25, Some(c"KEY"), Some(c"key")),
    (26, Some(c"PX"), Some(c"mapping information")),
    (
        27,
        Some(c"GPOS"),
        Some(c"geographical position (withdrawn)"),
    ),
    (28, Some(c"AAAA"), Some(c"IPv6 address")),
    (29, Some(c"LOC"), Some(c"location")),
    (30, Some(c"NXT"), Some(c"next valid name (unimplemented)")),
    (
        31,
        Some(c"EID"),
        Some(c"endpoint identifier (unimplemented)"),
    ),
    (32, Some(c"NIMLOC"), Some(c"NIMROD locator (unimplemented)")),
    (33, Some(c"SRV"), Some(c"server selection")),
    (34, Some(c"ATMA"), Some(c"ATM address (unimplemented)")),
    (35, Some(c"NAPTR"), Some(c"naptr")),
    (36, Some(c"KX"), Some(c"key exchange")),
    (37, Some(c"CERT"), Some(c"certificate")),
    (38, Some(c"A"), Some(c"IPv6 address (experminental)")),
    (39, Some(c"DNAME"), Some(c"non-terminal redirection")),
    (41, Some(c"OPT"), Some(c"opt")),
    (42, Some(c"apl"), Some(c"apl")),
    (43, Some(c"DS"), Some(c"delegation signer")),
    (44, Some(c"SSFP"), Some(c"SSH fingerprint")),
    (45, Some(c"IPSECKEY"), Some(c"IPSEC key")),
    (46, Some(c"RRSIG"), Some(c"rrsig")),
    (47, Some(c"NSEC"), Some(c"nsec")),
    (48, Some(c"DNSKEY"), Some(c"DNS key")),
    (
        49,
        Some(c"DHCID"),
        Some(c"dynamic host configuration identifier"),
    ),
    (50, Some(c"NSEC3"), Some(c"nsec3")),
    (51, Some(c"NSEC3PARAM"), Some(c"NSEC3 parameters")),
    (55, Some(c"HIP"), Some(c"host identity protocol")),
    (99, Some(c"SPF"), Some(c"sender policy framework")),
    (249, Some(c"TKEY"), Some(c"tkey")),
    (250, Some(c"TSIG"), Some(c"transaction signature")),
    (251, Some(c"IXFR"), Some(c"incremental zone transfer")),
    (252, Some(c"AXFR"), Some(c"zone transfer")),
    (256, Some(c"ZXFR"), Some(c"compressed zone transfer")),
    (
        253,
        Some(c"MAILB"),
        Some(c"mailbox-related data (deprecated)"),
    ),
    (254, Some(c"MAILA"), Some(c"mail agent (deprecated)")),
    (35, Some(c"NAPTR"), Some(c"URN Naming Authority")),
    (36, Some(c"KX"), Some(c"Key Exchange")),
    (37, Some(c"CERT"), Some(c"Certificate")),
    (38, Some(c"A6"), Some(c"IPv6 Address")),
    (39, Some(c"DNAME"), Some(c"dname")),
    (40, Some(c"SINK"), Some(c"Kitchen Sink (experimental)")),
    (41, Some(c"OPT"), Some(c"EDNS Options")),
    (255, Some(c"ANY"), Some(c"\"any\"")),
    (32769, Some(c"DLV"), Some(c"DNSSEC look-aside validation")),
    (0, None, None),
]));

/// bionic's `__stack_chk_guard`, for code built to read the canary from a global rather than the
/// thread pointer. Set once, before any library loads, from the kernel's random bytes as glibc sets
/// its own guard, so both are the same value.
static STACK_CHK_GUARD: AtomicUsize = AtomicUsize::new(0);

pub(super) fn init_stack_guard() {
    // SAFETY: getauxval has no preconditions; AT_RANDOM, when present, points at 16 random bytes
    // the kernel placed on the initial stack, which stay for the life of the process.
    let guard = unsafe {
        let random = libc::getauxval(libc::AT_RANDOM) as *const usize;
        match random.is_null() {
            true => 0,
            false => random.read_unaligned(),
        }
    };
    // glibc clears the low byte, so a string overflow cannot reproduce the canary.
    STACK_CHK_GUARD.store(guard & !0xff, Ordering::Relaxed);
}

/// BSD `getopt`'s request to start over. glibc's `getopt`, the only one apps reach, ignores it.
static OPTRESET: AtomicI32 = AtomicI32::new(0);

/// GNU `error`'s settings and count, bionic's own variables: `error` itself is not audited, so
/// nothing reads them yet.
static ERROR_MESSAGE_COUNT: AtomicU32 = AtomicU32::new(0);
static ERROR_ONE_PER_LINE: AtomicI32 = AtomicI32::new(0);
static ERROR_PRINT_PROGNAME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// bionic's x86_64 `__fe_dfl_env`, the `fenv_t` that `FE_DFL_ENV` points at: the x87 environment
/// as `fnstenv` stores it (control word 0x37f, empty status, every register empty) and MXCSR
/// 0x1f80. glibc's `fesetenv` loads the same 32-byte layout.
#[cfg(target_arch = "x86_64")]
static FE_DFL_ENV: [u32; 8] = [
    0xffff_037f,
    0xffff_0000,
    0xffff_ffff,
    0,
    0,
    0,
    0xffff_0000,
    0x1f80,
];

/// bionic's arm64 `__fe_dfl_env`: FPCR and FPSR both 0. Guest code on an x86_64 host gets it too.
pub(crate) static FE_DFL_ENV_ARM64: [u32; 2] = [0, 0];

#[cfg(target_arch = "aarch64")]
use FE_DFL_ENV_ARM64 as FE_DFL_ENV;

/// libstdc++'s `std::nothrow`, an empty object whose address is all that is used.
static NOTHROW: u8 = 0;

/// Each object by its symbol, for the shim table.
pub(super) fn objects() -> [(&'static str, *const ()); 12] {
    fn address<T>(object: &'static T) -> *const () {
        std::ptr::from_ref(object).cast()
    }
    [
        ("sys_signame", address(&SYS_SIGNAME)),
        ("sys_siglist", address(&SYS_SIGLIST)),
        ("__p_class_syms", address(&P_CLASS_SYMS)),
        ("__p_type_syms", address(&P_TYPE_SYMS)),
        ("__stack_chk_guard", address(&STACK_CHK_GUARD)),
        ("optreset", address(&OPTRESET)),
        ("error_message_count", address(&ERROR_MESSAGE_COUNT)),
        ("error_one_per_line", address(&ERROR_ONE_PER_LINE)),
        ("error_print_progname", address(&ERROR_PRINT_PROGNAME)),
        ("__fe_dfl_env", address(&FE_DFL_ENV)),
        ("_ZSt7nothrow", address(&NOTHROW)),
        ("_ctype_", address(&super::CTYPE)),
    ]
}
