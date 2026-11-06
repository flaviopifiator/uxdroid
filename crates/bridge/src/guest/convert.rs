//! Calls whose arguments mean something else to arm64 bionic than to x86_64 bionic, which is what
//! the host side serves: struct layouts (`stat`, `epoll_event`), open flags, `jmp_buf`, signal
//! handlers, `long double` and termios. Each converts the guest's arguments, calls the host
//! function x86_64 bionic code would get (a shim or an audited glibc function), and converts back.
//!
//! The arm64 layouts are bionic's, checked against the NDK's headers with clang's record layout
//! dump (`libc_table.py` finds which differ).

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::Mutex;

use super::{Cpu, Kind, callback, fail, native};

/// A host function by name, the one x86_64 bionic code binds to.
fn host(cpu: &dyn Cpu, name: &str) -> usize {
    let address = CString::new(name)
        .ok()
        .and_then(|text| crate::shims::resolve(&text, false));
    match address {
        Some(address) => address,
        None => fail(cpu, format_args!("the host has no {name}")),
    }
}

/// A host function pointer of type `F` for `name`.
///
/// # Safety
/// `F` must be the type of the host's `name`.
unsafe fn host_fn<F: Copy>(cpu: &dyn Cpu, name: &str) -> F {
    let address = host(cpu, name);
    assert_eq!(size_of::<F>(), size_of::<usize>());
    // SAFETY: the caller's contract.
    unsafe { std::mem::transmute_copy(&address) }
}

fn set_i32(cpu: &dyn Cpu, value: i32) {
    cpu.set_x(0, value as i64 as u64);
}

fn set_errno(value: c_int) {
    // SAFETY: errno is this thread's.
    unsafe { *libc::__errno_location() = value };
}

fn ptr<T>(value: u64) -> *mut T {
    value as usize as *mut T
}

// --- Open flags ---

/// arm64's values of the flags whose bits differ, then x86_64's (the generic ones).
const FLAG_PAIRS: [(i32, i32); 4] = [
    (0o40000, 0o200000),  // O_DIRECTORY
    (0o100000, 0o400000), // O_NOFOLLOW
    (0o200000, 0o40000),  // O_DIRECT
    (0o400000, 0o100000), // O_LARGEFILE
];

fn translate(flags: i32, from_guest: bool) -> i32 {
    let (all_from, mut out) = (
        FLAG_PAIRS.iter().fold(0, |all, &(guest, host)| {
            all | if from_guest { guest } else { host }
        }),
        0,
    );
    out |= flags & !all_from;
    for &(guest, host) in &FLAG_PAIRS {
        let (from, to) = if from_guest {
            (guest, host)
        } else {
            (host, guest)
        };
        if flags & from != 0 {
            out |= to;
        }
    }
    out
}

pub(super) fn host_flags(guest: i32) -> i32 {
    translate(guest, true)
}

pub(super) fn guest_flags(host: i32) -> i32 {
    translate(host, false)
}

type OpenAt = unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int;
type Open = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;

fn open(cpu: &dyn Cpu) {
    // SAFETY: bionic's open, called with the guest's path and mode and translated flags.
    let fd = unsafe {
        host_fn::<Open>(cpu, "open")(ptr(cpu.x(0)), host_flags(cpu.x(1) as i32), cpu.x(2) as u32)
    };
    set_i32(cpu, fd);
}

fn openat(cpu: &dyn Cpu) {
    // SAFETY: as for `open`.
    let fd = unsafe {
        host_fn::<OpenAt>(cpu, "openat")(
            cpu.x(0) as i32,
            ptr(cpu.x(1)),
            host_flags(cpu.x(2) as i32),
            cpu.x(3) as u32,
        )
    };
    set_i32(cpu, fd);
}

/// FORTIFY's `__open_2(path, flags)`, which never creates: no mode.
fn open_2(cpu: &dyn Cpu) {
    // SAFETY: as for `open`.
    let fd = unsafe {
        host_fn::<unsafe extern "C" fn(*const c_char, c_int) -> c_int>(cpu, "__open_2")(
            ptr(cpu.x(0)),
            host_flags(cpu.x(1) as i32),
        )
    };
    set_i32(cpu, fd);
}

fn openat_2(cpu: &dyn Cpu) {
    type OpenAt2 = unsafe extern "C" fn(c_int, *const c_char, c_int) -> c_int;
    // SAFETY: as for `open`.
    let fd = unsafe {
        host_fn::<OpenAt2>(cpu, "__openat_2")(
            cpu.x(0) as i32,
            ptr(cpu.x(1)),
            host_flags(cpu.x(2) as i32),
        )
    };
    set_i32(cpu, fd);
}

fn fcntl(cpu: &dyn Cpu) {
    type Fcntl = unsafe extern "C" fn(c_int, c_int, ...) -> c_int;
    let (fd, command) = (cpu.x(0) as i32, cpu.x(1) as i32);
    let argument = match command {
        libc::F_SETFL => host_flags(cpu.x(2) as i32) as i64 as u64,
        _ => cpu.x(2),
    };
    // SAFETY: bionic's fcntl; every other command's argument (an int, or a struct flock, whose
    // layout is the same) passes through.
    let result = unsafe { host_fn::<Fcntl>(cpu, "fcntl")(fd, command, argument) };
    set_i32(
        cpu,
        match command {
            libc::F_GETFL if result >= 0 => guest_flags(result),
            _ => result,
        },
    );
}

fn pipe2(cpu: &dyn Cpu) {
    // SAFETY: bionic's pipe2 with the guest's array of two ints.
    let status = unsafe {
        host_fn::<unsafe extern "C" fn(*mut c_int, c_int) -> c_int>(cpu, "pipe2")(
            ptr(cpu.x(0)),
            host_flags(cpu.x(1) as i32),
        )
    };
    set_i32(cpu, status);
}

fn mkostemp(cpu: &dyn Cpu) {
    // SAFETY: bionic's mkostemp with the guest's template.
    let fd = unsafe {
        host_fn::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(cpu, "mkostemp")(
            ptr(cpu.x(0)),
            host_flags(cpu.x(1) as i32),
        )
    };
    set_i32(cpu, fd);
}

fn mkostemps(cpu: &dyn Cpu) {
    // SAFETY: bionic's mkostemps with the guest's template and suffix length.
    let fd = unsafe {
        host_fn::<unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int>(cpu, "mkostemps")(
            ptr(cpu.x(0)),
            cpu.x(1) as i32,
            host_flags(cpu.x(2) as i32),
        )
    };
    set_i32(cpu, fd);
}

// --- stat ---

/// Bytes of arm64's `struct stat` and `struct stat64`, which are the same.
pub(super) const GUEST_STAT_BYTES: usize = 128;

/// arm64's `struct stat` from the host's: dev, ino, mode, nlink (32 bits), uid, gid, rdev, a pad,
/// size, blksize (32 bits), a pad, blocks, then three timespecs at 72, 88 and 104.
pub(super) fn guest_stat(host: &libc::stat) -> [u8; GUEST_STAT_BYTES] {
    let mut out = [0u8; GUEST_STAT_BYTES];
    let mut put =
        |offset: usize, bytes: &[u8]| out[offset..offset + bytes.len()].copy_from_slice(bytes);
    put(0, &host.st_dev.to_le_bytes());
    put(8, &host.st_ino.to_le_bytes());
    put(16, &host.st_mode.to_le_bytes());
    put(20, &(host.st_nlink as u32).to_le_bytes());
    put(24, &host.st_uid.to_le_bytes());
    put(28, &host.st_gid.to_le_bytes());
    put(32, &host.st_rdev.to_le_bytes());
    put(48, &host.st_size.to_le_bytes());
    put(56, &(host.st_blksize as i32).to_le_bytes());
    put(64, &host.st_blocks.to_le_bytes());
    put(72, &host.st_atime.to_le_bytes());
    put(80, &host.st_atime_nsec.to_le_bytes());
    put(88, &host.st_mtime.to_le_bytes());
    put(96, &host.st_mtime_nsec.to_le_bytes());
    put(104, &host.st_ctime.to_le_bytes());
    put(112, &host.st_ctime_nsec.to_le_bytes());
    out
}

/// Copies a host stat into the guest's buffer when the call succeeded.
fn finish_stat(cpu: &dyn Cpu, status: c_int, info: &libc::stat, buffer: u64) {
    if status == 0 {
        let bytes = guest_stat(info);
        // SAFETY: the guest passes a writable struct stat, 128 bytes on arm64.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr(buffer), GUEST_STAT_BYTES);
        }
    }
    set_i32(cpu, status);
}

fn zeroed_stat() -> libc::stat {
    // SAFETY: zero is a valid struct stat.
    unsafe { std::mem::zeroed() }
}

fn stat_by(cpu: &dyn Cpu, name: &str) {
    type Stat = unsafe extern "C" fn(*const c_char, *mut libc::stat) -> c_int;
    let mut info = zeroed_stat();
    // SAFETY: the host's stat or lstat, which fills an x86_64 struct stat.
    let status = unsafe { host_fn::<Stat>(cpu, name)(ptr(cpu.x(0)), &mut info) };
    finish_stat(cpu, status, &info, cpu.x(1));
}

fn stat(cpu: &dyn Cpu) {
    stat_by(cpu, "stat");
}

fn lstat(cpu: &dyn Cpu) {
    stat_by(cpu, "lstat");
}

fn fstat(cpu: &dyn Cpu) {
    type Fstat = unsafe extern "C" fn(c_int, *mut libc::stat) -> c_int;
    let mut info = zeroed_stat();
    // SAFETY: the host's fstat, which fills an x86_64 struct stat.
    let status = unsafe { host_fn::<Fstat>(cpu, "fstat")(cpu.x(0) as i32, &mut info) };
    finish_stat(cpu, status, &info, cpu.x(1));
}

fn fstatat(cpu: &dyn Cpu) {
    type Fstatat = unsafe extern "C" fn(c_int, *const c_char, *mut libc::stat, c_int) -> c_int;
    let mut info = zeroed_stat();
    // SAFETY: the host's fstatat; its AT_ flags have the same values on both.
    let status = unsafe {
        host_fn::<Fstatat>(cpu, "fstatat")(
            cpu.x(0) as i32,
            ptr(cpu.x(1)),
            &mut info,
            cpu.x(3) as i32,
        )
    };
    finish_stat(cpu, status, &info, cpu.x(2));
}

// --- epoll ---

/// x86_64's `struct epoll_event`, packed; arm64's is 16 bytes with the data at 8.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct HostEvent {
    events: u32,
    data: u64,
}

const GUEST_EVENT_BYTES: u64 = 16;
/// Events taken per wait; a guest asking for more gets them over several waits.
const MAX_EVENTS: usize = 1024;

fn epoll_ctl(cpu: &dyn Cpu) {
    type Ctl = unsafe extern "C" fn(c_int, c_int, c_int, *mut HostEvent) -> c_int;
    let guest = cpu.x(3);
    let mut event = HostEvent::default();
    let event_ptr = if guest == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: the guest passes its struct epoll_event, 16 readable bytes.
        unsafe {
            event.events = native::read_u64(guest) as u32;
            event.data = native::read_u64(guest + 8);
        }
        &mut event
    };
    // SAFETY: the host's epoll_ctl with a host-layout event.
    let status = unsafe {
        host_fn::<Ctl>(cpu, "epoll_ctl")(
            cpu.x(0) as i32,
            cpu.x(1) as i32,
            cpu.x(2) as i32,
            event_ptr,
        )
    };
    set_i32(cpu, status);
}

/// `epoll_wait`, or `epoll_pwait` with its signal mask, which is the kernel's 8 bytes on both.
fn wait_events(cpu: &dyn Cpu, with_mask: bool) {
    type Wait = unsafe extern "C" fn(c_int, *mut HostEvent, c_int, c_int) -> c_int;
    type PWait = unsafe extern "C" fn(c_int, *mut HostEvent, c_int, c_int, *const c_void) -> c_int;
    let requested = cpu.x(2) as i32;
    let count = usize::try_from(requested).unwrap_or(0).clamp(1, MAX_EVENTS);
    let asked = if requested > 0 {
        count as c_int
    } else {
        requested
    };
    let mut events = vec![HostEvent::default(); count];
    // SAFETY: the host's epoll_wait or epoll_pwait with a buffer of `count` host events; a
    // non-positive count is passed on for the host to refuse.
    let ready = unsafe {
        if with_mask {
            host_fn::<PWait>(cpu, "epoll_pwait")(
                cpu.x(0) as i32,
                events.as_mut_ptr(),
                asked,
                cpu.x(3) as i32,
                ptr(cpu.x(4)),
            )
        } else {
            host_fn::<Wait>(cpu, "epoll_wait")(
                cpu.x(0) as i32,
                events.as_mut_ptr(),
                asked,
                cpu.x(3) as i32,
            )
        }
    };
    let out = cpu.x(1);
    for (index, event) in events
        .iter()
        .take(usize::try_from(ready).unwrap_or(0))
        .enumerate()
    {
        let at = out + GUEST_EVENT_BYTES * index as u64;
        let (events_bits, data) = (event.events, event.data);
        // SAFETY: the guest passes room for `requested` 16-byte events; at most that many are ready.
        unsafe {
            native::write_u64(at, u64::from(events_bits));
            native::write_u64(at + 8, data);
        }
    }
    set_i32(cpu, ready);
}

fn epoll_wait(cpu: &dyn Cpu) {
    wait_events(cpu, false);
}

fn epoll_pwait(cpu: &dyn Cpu) {
    wait_events(cpu, true);
}

// --- ioctl and termios ---

/// Requests reach the host's ioctl with their one argument unchanged: the kernel's numbers and
/// argument layouts are the same for arm64 and x86_64 LP64 code (FIONREAD, termios, `struct ifreq`).
fn ioctl_raw(cpu: &dyn Cpu, fd: i32, request: u64, argument: u64) -> c_int {
    type Ioctl = unsafe extern "C" fn(c_int, libc::c_ulong, ...) -> c_int;
    // SAFETY: the host's ioctl with the guest's request and argument, both LP64 Linux.
    unsafe { host_fn::<Ioctl>(cpu, "ioctl")(fd, request as libc::c_ulong, argument) }
}

fn ioctl(cpu: &dyn Cpu) {
    let status = ioctl_raw(cpu, cpu.x(0) as i32, cpu.x(1) as u32 as u64, cpu.x(2));
    set_i32(cpu, status);
}

const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;

fn tcgetattr(cpu: &dyn Cpu) {
    let status = ioctl_raw(cpu, cpu.x(0) as i32, TCGETS, cpu.x(1));
    set_i32(cpu, status);
}

/// TCSANOW, TCSADRAIN and TCSAFLUSH are TCSETS, TCSETSW and TCSETSF.
fn tcsetattr(cpu: &dyn Cpu) {
    let action = cpu.x(1) as i32;
    if !(0..=2).contains(&action) {
        set_errno(libc::EINVAL);
        set_i32(cpu, -1);
        return;
    }
    let status = ioctl_raw(cpu, cpu.x(0) as i32, TCSETS + action as u64, cpu.x(2));
    set_i32(cpu, status);
}

/// The kernel's termios: four flag words, then the line discipline and 19 control characters.
fn cfmakeraw(cpu: &dyn Cpu) {
    let termios = cpu.x(0);
    let update = |offset: u64, clear: u32, set: u32| {
        // SAFETY: the guest passes its struct termios; each flag word is 4 bytes at `offset`.
        unsafe {
            let word = ptr::<u32>(termios + offset);
            word.write_unaligned((word.read_unaligned() & !clear) | set);
        }
    };
    let input = libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON;
    update(0, input, 0);
    update(4, libc::OPOST, 0);
    update(8, libc::CSIZE | libc::PARENB, libc::CS8);
    update(
        12,
        libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN,
        0,
    );
    // SAFETY: c_cc starts at 17; VTIME is 5 and VMIN 6 in the kernel's numbering.
    unsafe {
        *ptr::<u8>(termios + 17 + 5) = 0;
        *ptr::<u8>(termios + 17 + 6) = 1;
    }
}

// --- setjmp and longjmp, which never leave guest code ---

/// Words of the guest's `jmp_buf` used: a mark, the thread and depth it was made at, whether a
/// signal mask was saved and the mask, x19 to x30, sp, and d8 to d15. bionic's is 32 words.
const JMP_MARK: u64 = 0x7578_6472_6F69_646A;
const JMP_WORDS: u64 = 26;

fn save_jump(cpu: &dyn Cpu, save_mask: bool) {
    let buffer = cpu.x(0);
    let (thread, depth) = super::guest_context();
    let mut mask = 0u64;
    if save_mask {
        type Mask = unsafe extern "C" fn(c_int, *const u64, *mut u64) -> c_int;
        // SAFETY: bionic's sigprocmask, reading the current mask into one 64-bit word.
        unsafe {
            host_fn::<Mask>(cpu, "sigprocmask")(libc::SIG_BLOCK, std::ptr::null(), &mut mask)
        };
    }
    let mut words = vec![JMP_MARK, thread, depth as u64, u64::from(save_mask), mask];
    words.extend((19..=30).map(|register| cpu.x(register)));
    words.push(cpu.sp());
    words.extend((8..16).map(|register| cpu.v(register)[0]));
    debug_assert_eq!(words.len() as u64, JMP_WORDS);
    for (index, &word) in words.iter().enumerate() {
        // SAFETY: the guest passes its jmp_buf, 32 writable words on arm64.
        unsafe { native::write_u64(buffer + 8 * index as u64, word) };
    }
    cpu.set_x(0, 0);
}

fn setjmp(cpu: &dyn Cpu) {
    save_jump(cpu, true);
}

fn underscore_setjmp(cpu: &dyn Cpu) {
    save_jump(cpu, false);
}

fn sigsetjmp(cpu: &dyn Cpu) {
    save_jump(cpu, cpu.x(1) != 0);
}

/// Puts back what setjmp saved; the stub's `ret` then returns from setjmp again, with `value`.
fn longjmp(cpu: &dyn Cpu) {
    let buffer = cpu.x(0);
    // SAFETY: the guest passes the jmp_buf it gave setjmp, 32 readable words.
    let word = |index: u64| unsafe { native::read_u64(buffer + 8 * index) };
    let (thread, depth) = super::guest_context();
    if word(0) != JMP_MARK || word(1) != thread {
        fail(
            cpu,
            format_args!("longjmp to a jmp_buf setjmp did not fill on this thread"),
        );
    }
    if word(2) != depth as u64 {
        fail(
            cpu,
            format_args!("longjmp across a host call, which would skip its frames"),
        );
    }
    if word(3) != 0 {
        type Mask = unsafe extern "C" fn(c_int, *const u64, *mut u64) -> c_int;
        let mask = word(4);
        // SAFETY: bionic's sigprocmask, restoring the saved 64-bit mask.
        unsafe {
            host_fn::<Mask>(cpu, "sigprocmask")(libc::SIG_SETMASK, &mask, std::ptr::null_mut())
        };
    }
    for (offset, register) in (19..=30).enumerate() {
        cpu.set_x(register, word(5 + offset as u64));
    }
    cpu.set_sp(word(17));
    for (offset, register) in (8..16).enumerate() {
        cpu.set_v(register, [word(18 + offset as u64), 0]);
    }
    let value = cpu.x(1) as i32;
    set_i32(cpu, if value == 0 { 1 } else { value });
}

// --- Signal handlers ---

/// Signals the host itself uses for faults: ART's implicit null checks and the engine's fast
/// memory both take SIGSEGV first and chain, and a fault in translated code is not yet mapped
/// back to the guest's PC. Setting a guest action for one fails with EINVAL and says so once; a
/// query reads back the default action.
const FAULT_SIGNALS: [c_int; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGTRAP,
];
const SA_SIGINFO: u32 = 4;
const SA_RESTORER: u32 = 0x0400_0000;
const SIGNALS: usize = 65;

/// bionic's LP64 `struct sigaction`, the same on arm64 and x86_64: flags, handler, mask, restorer.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Action {
    flags: u32,
    padding: u32,
    handler: u64,
    mask: u64,
    restorer: u64,
}

/// What each signal's guest handler is and the host thunk installed for it.
static ACTIONS: Mutex<[Option<(Action, u64)>; SIGNALS]> = Mutex::new([None; SIGNALS]);

fn read_action(address: u64) -> Option<Action> {
    if address == 0 {
        return None;
    }
    // SAFETY: the guest passes its struct sigaction, 32 readable bytes.
    Some(unsafe { std::ptr::read_unaligned(ptr::<Action>(address)) })
}

fn write_action(address: u64, action: Action) {
    if address != 0 {
        // SAFETY: the guest passes room for a struct sigaction.
        unsafe { std::ptr::write_unaligned(ptr::<Action>(address), action) };
    }
}

/// `sigaction` for guest code: a guest handler becomes a host thunk that runs it, with no
/// `ucontext_t` (the host's is x86-64's); host handlers and SIG_DFL or SIG_IGN pass unchanged.
fn sigaction(cpu: &dyn Cpu) {
    let signal = cpu.x(0) as i32;
    let Some(slot) = usize::try_from(signal)
        .ok()
        .filter(|&slot| (1..SIGNALS).contains(&slot))
    else {
        set_errno(libc::EINVAL);
        set_i32(cpu, -1);
        return;
    };
    let wanted = read_action(cpu.x(1));
    let mut actions = ACTIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let recorded = actions[slot];

    if FAULT_SIGNALS.contains(&signal) {
        write_action(cpu.x(2), Action::default());
        if wanted.is_some() {
            static SAID: Mutex<u64> = Mutex::new(0);
            let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *said & (1 << slot) == 0 {
                *said |= 1 << slot;
                eprintln!(
                    "uxdroid: arm64 code cannot set an action for signal {signal}, which the host handles"
                );
            }
            set_errno(libc::EINVAL);
            set_i32(cpu, -1);
        } else {
            set_i32(cpu, 0);
        }
        return;
    }

    let mut thunk = 0;
    let host_action = wanted.map(|action| {
        let mut host_action = action;
        host_action.flags &= !SA_RESTORER;
        host_action.restorer = 0;
        if action.handler > 1 && crate::loader::is_guest_address(action.handler as usize) {
            let signature = if action.flags & SA_SIGINFO != 0 {
                "void(i32,ptr,ptr)"
            } else {
                "void(i32)"
            };
            thunk = match callback(action.handler, signature, Kind::Signal) {
                Ok(made) => made as u64,
                Err(reason) => fail(cpu, format_args!("sigaction: {reason}")),
            };
            host_action.handler = thunk;
        }
        host_action
    });
    let mut previous = Action::default();
    type Sigaction = unsafe extern "C" fn(c_int, *const Action, *mut Action) -> c_int;
    // SAFETY: bionic's sigaction with bionic's LP64 struct, whose handler is a host function.
    let status = unsafe {
        host_fn::<Sigaction>(cpu, "sigaction")(
            signal,
            host_action
                .as_ref()
                .map_or(std::ptr::null(), std::ptr::from_ref),
            &mut previous,
        )
    };
    if status == 0 {
        let previous = match recorded {
            Some((action, installed)) if installed != 0 && installed == previous.handler => action,
            _ => previous,
        };
        write_action(cpu.x(2), previous);
        if let Some(action) = wanted {
            actions[slot] = (thunk != 0).then_some((action, thunk));
        }
    }
    set_i32(cpu, status);
}

// --- long double: binary128 on arm64, x87's 80 bits on the host ---

/// The binary128 bits of `value`, low word first: exact, since every double is a binary128.
pub(super) fn quad_from_double(value: f64) -> [u64; 2] {
    let bits = value.to_bits();
    let sign = bits >> 63;
    let exponent = ((bits >> 52) & 0x7FF) as i64;
    let mut mantissa = bits & ((1 << 52) - 1);
    let quad_exponent = match exponent {
        0 if mantissa == 0 => 0,
        0 => {
            // Subnormal: normalize, as binary128's wider exponent holds it.
            let shift = mantissa.leading_zeros() as i64 - 11;
            mantissa = (mantissa << shift) & ((1 << 52) - 1);
            1 - 1023 - shift + 16383
        }
        0x7FF => 0x7FFF,
        _ => exponent - 1023 + 16383,
    } as u64;
    // The 52 mantissa bits become the top of binary128's 112: 48 in the high word, 4 in the low.
    let high = (sign << 63) | (quad_exponent << 48) | (mantissa >> 4);
    let low = mantissa << 60;
    [low, high]
}

fn long_double_by(cpu: &dyn Cpu, double: &str, arguments: usize) {
    type Two = unsafe extern "C" fn(u64, u64) -> f64;
    type Three = unsafe extern "C" fn(u64, u64, u64) -> f64;
    // SAFETY: the host's strtod, strtod_l or wcstod with the guest's arguments.
    let value = unsafe {
        match arguments {
            2 => host_fn::<Two>(cpu, double)(cpu.x(0), cpu.x(1)),
            _ => host_fn::<Three>(cpu, double)(cpu.x(0), cpu.x(1), cpu.x(2)),
        }
    };
    cpu.set_v(0, quad_from_double(value));
}

/// Parsed at double precision: digits past a double's 17 are lost.
fn strtold(cpu: &dyn Cpu) {
    long_double_by(cpu, "strtod", 2);
}

fn strtold_l(cpu: &dyn Cpu) {
    long_double_by(cpu, "strtod_l", 3);
}

fn wcstold(cpu: &dyn Cpu) {
    long_double_by(cpu, "wcstod", 2);
}

// --- Other callbacks ---

fn thunk_or_null(cpu: &dyn Cpu, target: u64, signature: &str) -> usize {
    if target == 0 {
        return 0;
    }
    match callback(target, signature, Kind::Plain) {
        Ok(thunk) => thunk,
        Err(reason) => fail(cpu, format_args!("{reason}")),
    }
}

/// Destructors of C++ `thread_local` objects, run by the host when the thread exits.
fn cxa_thread_atexit_impl(cpu: &dyn Cpu) {
    type AtExit = unsafe extern "C" fn(usize, u64, u64) -> c_int;
    let destructor = thunk_or_null(cpu, cpu.x(0), "void(ptr)");
    // SAFETY: the host's __cxa_thread_atexit_impl with a host thunk of the destructor.
    let status = unsafe {
        host_fn::<AtExit>(cpu, "__cxa_thread_atexit_impl")(destructor, cpu.x(1), cpu.x(2))
    };
    set_i32(cpu, status);
}

fn pthread_atfork(cpu: &dyn Cpu) {
    type AtFork = unsafe extern "C" fn(usize, usize, usize) -> c_int;
    let [prepare, parent, child] =
        [0, 1, 2].map(|index| thunk_or_null(cpu, cpu.x(index), "void()"));
    // SAFETY: the host's pthread_atfork with host thunks or null.
    let status = unsafe { host_fn::<AtFork>(cpu, "pthread_atfork")(prepare, parent, child) };
    set_i32(cpu, status);
}

// --- The dynamic linker ---

fn dlopen(cpu: &dyn Cpu) {
    // SAFETY: the loader's dlopen with the guest's C string; the handle is opaque to the guest.
    let handle = unsafe { crate::loader::dlopen(ptr(cpu.x(0)), cpu.x(1) as i32) };
    cpu.set_x(0, handle as u64);
}

fn dlsym(cpu: &dyn Cpu) {
    // SAFETY: the libdl contract makes the name a C string; the read is bounded.
    let name = unsafe { native::c_string(cpu.x(1), 4096) };
    let found = crate::loader::guest_symbol(ptr(cpu.x(0)), name).unwrap_or(0);
    cpu.set_x(0, found);
}

fn dlerror(cpu: &dyn Cpu) {
    cpu.set_x(0, crate::loader::dlerror() as u64);
}

fn dlclose(cpu: &dyn Cpu) {
    set_i32(cpu, crate::loader::dlclose(ptr(cpu.x(0))));
}

/// `Dl_info` is four pointers on both.
fn dladdr(cpu: &dyn Cpu) {
    // SAFETY: the loader's dladdr with the guest's address and writable Dl_info.
    let found = unsafe { crate::loader::dladdr(ptr(cpu.x(0)), ptr(cpu.x(1))) };
    set_i32(cpu, found);
}

/// GL and EGL functions by name, from the signature tables, so each is a guest stub; a name no
/// table signs is null, as for a driver without that extension.
fn egl_get_proc_address(cpu: &dyn Cpu) {
    // SAFETY: eglGetProcAddress takes a C string; the read is bounded.
    let name = unsafe { native::c_string(cpu.x(0), 4096) };
    let tables = crate::table::all();
    let known = tables.iter().any(|table| table.find(name).is_some());
    let stub = match known {
        true => super::import(name, &tables, false)
            .ok()
            .flatten()
            .unwrap_or(0),
        false => 0,
    };
    cpu.set_x(0, stub);
}

pub(super) const SPECIALS: &[(&str, super::libc::Special)] = &[
    ("dlopen", dlopen),
    ("android_dlopen_ext", dlopen),
    ("dlsym", dlsym),
    ("dlerror", dlerror),
    ("dlclose", dlclose),
    ("dladdr", dladdr),
    ("eglGetProcAddress", egl_get_proc_address),
    ("open", open),
    ("open64", open),
    ("openat", openat),
    ("openat64", openat),
    ("__open_2", open_2),
    ("__openat_2", openat_2),
    ("fcntl", fcntl),
    ("fcntl64", fcntl),
    ("pipe2", pipe2),
    ("mkostemp", mkostemp),
    ("mkostemp64", mkostemp),
    ("mkostemps", mkostemps),
    ("mkostemps64", mkostemps),
    ("stat", stat),
    ("stat64", stat),
    ("lstat", lstat),
    ("lstat64", lstat),
    ("fstat", fstat),
    ("fstat64", fstat),
    ("fstatat", fstatat),
    ("fstatat64", fstatat),
    ("epoll_ctl", epoll_ctl),
    ("epoll_wait", epoll_wait),
    ("epoll_pwait", epoll_pwait),
    ("epoll_pwait64", epoll_pwait),
    ("ioctl", ioctl),
    ("tcgetattr", tcgetattr),
    ("tcsetattr", tcsetattr),
    ("cfmakeraw", cfmakeraw),
    ("setjmp", setjmp),
    ("_setjmp", underscore_setjmp),
    ("sigsetjmp", sigsetjmp),
    ("longjmp", longjmp),
    ("_longjmp", longjmp),
    ("siglongjmp", longjmp),
    ("sigaction", sigaction),
    ("sigaction64", sigaction),
    ("strtold", strtold),
    ("strtold_l", strtold_l),
    ("wcstold", wcstold),
    ("__cxa_thread_atexit_impl", cxa_thread_atexit_impl),
    ("pthread_atfork", pthread_atfork),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_flags_swap_the_four_bits_that_differ() {
        const O_RDWR: i32 = 2;
        const O_CLOEXEC: i32 = 0o2000000;
        let guest = O_RDWR | O_CLOEXEC | 0o40000 | 0o100000;
        let host = host_flags(guest);
        assert_eq!(
            host,
            O_RDWR | O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW
        );
        assert_eq!(guest_flags(host), guest);
        // The host kernel's O_LARGEFILE is arm64's O_LARGEFILE, not its O_NOFOLLOW.
        assert_eq!(guest_flags(0o100000), 0o400000);
        assert_eq!(host_flags(0o200000), libc::O_DIRECT);
    }

    #[test]
    fn stat_lands_at_arm64_s_offsets() {
        let mut host = zeroed_stat();
        host.st_ino = 0x0102;
        host.st_mode = 0o100644;
        host.st_nlink = 3;
        host.st_size = 5;
        host.st_blksize = 4096;
        host.st_mtime = 77;
        host.st_ctime_nsec = 9;
        let guest = guest_stat(&host);
        let word = |offset: usize, bytes: usize| {
            let mut value = [0u8; 8];
            value[..bytes].copy_from_slice(&guest[offset..offset + bytes]);
            u64::from_le_bytes(value)
        };
        assert_eq!(word(8, 8), 0x0102);
        assert_eq!(word(16, 4), 0o100644);
        assert_eq!(word(20, 4), 3);
        assert_eq!(word(48, 8), 5);
        assert_eq!(word(56, 4), 4096);
        assert_eq!(word(88, 8), 77);
        assert_eq!(word(112, 8), 9);
    }

    #[test]
    fn doubles_widen_to_binary128_exactly() {
        assert_eq!(quad_from_double(0.0), [0, 0]);
        assert_eq!(quad_from_double(1.0), [0, 0x3FFF_0000_0000_0000]);
        assert_eq!(quad_from_double(-2.5), [0, 0xC000_4000_0000_0000]);
        assert_eq!(quad_from_double(f64::INFINITY), [0, 0x7FFF_0000_0000_0000]);
        // The smallest subnormal double, 2^-1074, is normal in binary128.
        assert_eq!(
            quad_from_double(f64::from_bits(1)),
            [0, (16383 - 1074) << 48]
        );
        assert_eq!(
            quad_from_double(1.0 + f64::EPSILON),
            [1 << 60, 0x3FFF_0000_0000_0000]
        );
    }
}
