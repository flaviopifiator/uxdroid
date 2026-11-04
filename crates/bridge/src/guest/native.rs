//! The unsafe edge of the guest frame: guest memory, stubs, stacks, and the two assembly routines
//! that move registers between System V and the frames the rest of the module fills.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use super::{Callback, enter};

// --- Guest memory ---

/// # Safety
/// `address` must be 8 readable bytes.
pub(super) unsafe fn read_u64(address: u64) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { std::ptr::read_unaligned(address as usize as *const u64) }
}

/// # Safety
/// `address` must be 8 writable bytes nothing else uses.
pub(super) unsafe fn write_u64(address: u64, value: u64) {
    // SAFETY: the caller's contract.
    unsafe { std::ptr::write_unaligned(address as usize as *mut u64, value) }
}

/// The bytes of the C string at `address`, at most `limit` of them; empty for null.
///
/// # Safety
/// `address` must be null or a C string, or readable for `limit` bytes.
pub(super) unsafe fn c_string<'a>(address: u64, limit: usize) -> &'a [u8] {
    if address == 0 {
        return &[];
    }
    let start = address as usize as *const u8;
    let mut length = 0;
    // SAFETY: the caller's contract; the walk stops at the terminator or the limit.
    while length < limit && unsafe { *start.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the bytes just read.
    unsafe { std::slice::from_raw_parts(start, length) }
}

// --- Stubs: `svc #n; ret` for each service ---

const STUB_BYTES: usize = 8;

struct Region(usize);

fn stubs() -> Option<usize> {
    static STUBS: OnceLock<Option<Region>> = OnceLock::new();
    STUBS
        .get_or_init(|| {
            let length = super::MAX_SERVICES * STUB_BYTES;
            // SAFETY: a new anonymous mapping touches no existing memory. Guest code is read by
            // the engine, not executed by the host, so it needs no execute permission.
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            (address != libc::MAP_FAILED).then_some(Region(address as usize))
        })
        .as_ref()
        .map(|region| region.0)
}

/// Writes the stub of service `number` and returns its guest address. Each stub is written once,
/// before its address is handed out, so no translated copy of it can be stale.
pub(super) fn stub(number: u32) -> Option<u64> {
    const SVC: u32 = 0xD400_0001;
    const RET: u32 = 0xD65F_03C0;
    if number as usize >= super::MAX_SERVICES {
        return None;
    }
    let address = stubs()? + number as usize * STUB_BYTES;
    let mut code = [0u8; STUB_BYTES];
    code[..4].copy_from_slice(&(SVC | (number << 5)).to_le_bytes());
    code[4..].copy_from_slice(&RET.to_le_bytes());
    // SAFETY: the address is this service's own 8 bytes inside the mapping made above.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), address as *mut u8, STUB_BYTES) };
    Some(address as u64)
}

// --- Stacks and threads ---

const STACK_BYTES: usize = 4 << 20;
const GUARD_BYTES: usize = 64 << 10;

/// A guest stack with a guard region below it; pages are committed as they are touched.
pub(super) struct Stack {
    base: usize,
}

impl Stack {
    pub(super) fn new() -> Option<Stack> {
        // SAFETY: a new anonymous mapping touches no existing memory.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                STACK_BYTES + GUARD_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_STACK,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the guard is the low end of the mapping just made.
        if unsafe { libc::mprotect(base, GUARD_BYTES, libc::PROT_NONE) } != 0 {
            // SAFETY: the mapping just made, unused.
            unsafe { libc::munmap(base, STACK_BYTES + GUARD_BYTES) };
            return None;
        }
        Some(Stack {
            base: base as usize,
        })
    }

    pub(super) fn bottom(&self) -> u64 {
        (self.base + GUARD_BYTES) as u64
    }

    pub(super) fn top(&self) -> u64 {
        (self.base + GUARD_BYTES + STACK_BYTES) as u64
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made; the thread that used it is exiting.
        unsafe { libc::munmap(self.base as *mut c_void, STACK_BYTES + GUARD_BYTES) };
    }
}

pub(super) fn random_u64() -> u64 {
    let mut value = 0u64;
    // SAFETY: writes at most 8 bytes into `value`.
    let read = unsafe { libc::getrandom(std::ptr::from_mut(&mut value).cast(), 8, 0) };
    if read != 8 {
        value = 0x5EED_0000_0000_0000 ^ std::process::id() as u64;
    }
    value
}

/// Runs `destructor(value)` when the calling thread exits. A pthread key is used, not a Rust
/// thread local, because guest destructors that run later, also through pthread keys, may need
/// guest state again and must find it gone rather than destroyed.
pub(super) fn on_thread_exit(value: *mut c_void, destructor: extern "C" fn(*mut c_void)) {
    static KEY: OnceLock<Option<libc::pthread_key_t>> = OnceLock::new();
    let key = KEY.get_or_init(|| {
        let mut key = 0;
        // SAFETY: `key` is writable and the destructor has the required signature.
        let status = unsafe { libc::pthread_key_create(&mut key, Some(destructor)) };
        (status == 0).then_some(key)
    });
    if let Some(key) = *key {
        // SAFETY: the key was created above; the value is only passed to the destructor.
        unsafe { libc::pthread_setspecific(key, value) };
    }
}

// --- Host calls: guest arguments into System V registers ---

#[repr(C)]
#[derive(Default)]
pub(super) struct HostFrame {
    pub function: u64,
    pub gpr: [u64; 6],
    pub xmm: [u64; 8],
    stack: u64,
    stack_count: u64,
    pub rax: u64,
    pub rdx: u64,
    pub xmm0: u64,
}

std::arch::global_asm!(
    ".globl uxdroid_guest_host_call",
    ".hidden uxdroid_guest_host_call",
    ".type uxdroid_guest_host_call, @function",
    ".p2align 4",
    "uxdroid_guest_host_call:",
    ".cfi_startproc",
    "push rbp",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "mov rbp, rsp",
    ".cfi_def_cfa_register rbp",
    "push rbx",
    ".cfi_offset rbx, -24",
    "push r12",
    ".cfi_offset r12, -32",
    "mov rbx, rdi",
    "mov rcx, [rbx + 128]",
    "lea rax, [rcx * 8 + 15]",
    "and rax, -16",
    "sub rsp, rax",
    "mov rsi, [rbx + 120]",
    "xor edx, edx",
    "2:",
    "cmp rdx, rcx",
    "jae 3f",
    "mov rax, [rsi + rdx * 8]",
    "mov [rsp + rdx * 8], rax",
    "inc rdx",
    "jmp 2b",
    "3:",
    "movq xmm0, [rbx + 56]",
    "movq xmm1, [rbx + 64]",
    "movq xmm2, [rbx + 72]",
    "movq xmm3, [rbx + 80]",
    "movq xmm4, [rbx + 88]",
    "movq xmm5, [rbx + 96]",
    "movq xmm6, [rbx + 104]",
    "movq xmm7, [rbx + 112]",
    "mov rsi, [rbx + 16]",
    "mov rdx, [rbx + 24]",
    "mov rcx, [rbx + 32]",
    "mov r8, [rbx + 40]",
    "mov r9, [rbx + 48]",
    "mov rdi, [rbx + 8]",
    "mov r11, [rbx]",
    // An upper bound on the vector registers used, which variadic callees read.
    "mov eax, 8",
    "call r11",
    "mov [rbx + 136], rax",
    "mov [rbx + 144], rdx",
    "movq [rbx + 152], xmm0",
    "lea rsp, [rbp - 16]",
    "pop r12",
    "pop rbx",
    "pop rbp",
    ".cfi_def_cfa rsp, 8",
    "ret",
    ".cfi_endproc",
    ".size uxdroid_guest_host_call, . - uxdroid_guest_host_call",
);

unsafe extern "C" {
    fn uxdroid_guest_host_call(frame: *mut HostFrame);
}

/// Calls `frame.function` with the frame's registers and `stack` as its stack arguments.
///
/// # Safety
/// The function must accept exactly these arguments under System V.
pub(super) unsafe fn host_call(frame: &mut HostFrame, stack: &[u64]) {
    frame.stack = stack.as_ptr() as u64;
    frame.stack_count = stack.len() as u64;
    // SAFETY: the caller's contract; the routine reads the frame and the stack slice, which
    // outlive the call.
    unsafe { uxdroid_guest_host_call(frame) }
}

// --- Thunks: System V calls into guest code ---

/// The registers a thunk saved, and the result it returns.
#[repr(C)]
pub(super) struct EntryFrame {
    pub gpr: [u64; 6],
    pub xmm: [u64; 8],
    /// Where the caller's stack arguments start.
    pub stack: u64,
    pub rax: u64,
    pub xmm0: u64,
    padding: u64,
}

const _: () = assert!(size_of::<EntryFrame>() == 144);

std::arch::global_asm!(
    ".globl uxdroid_guest_entry",
    ".hidden uxdroid_guest_entry",
    ".type uxdroid_guest_entry, @function",
    ".p2align 4",
    "uxdroid_guest_entry:",
    ".cfi_startproc",
    "push rbp",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "mov rbp, rsp",
    ".cfi_def_cfa_register rbp",
    "sub rsp, 144",
    "mov [rsp], rdi",
    "mov [rsp + 8], rsi",
    "mov [rsp + 16], rdx",
    "mov [rsp + 24], rcx",
    "mov [rsp + 32], r8",
    "mov [rsp + 40], r9",
    "movq [rsp + 48], xmm0",
    "movq [rsp + 56], xmm1",
    "movq [rsp + 64], xmm2",
    "movq [rsp + 72], xmm3",
    "movq [rsp + 80], xmm4",
    "movq [rsp + 88], xmm5",
    "movq [rsp + 96], xmm6",
    "movq [rsp + 104], xmm7",
    "lea r10, [rbp + 16]",
    "mov [rsp + 112], r10",
    "mov rdi, rax",
    "mov rsi, rsp",
    "call {dispatch}",
    "mov rax, [rsp + 120]",
    "movq xmm0, [rsp + 128]",
    "mov rsp, rbp",
    "pop rbp",
    ".cfi_def_cfa rsp, 8",
    "ret",
    ".cfi_endproc",
    ".size uxdroid_guest_entry, . - uxdroid_guest_entry",
    dispatch = sym dispatch,
);

unsafe extern "C" {
    fn uxdroid_guest_entry();
}

extern "C" fn dispatch(context: *const Callback, frame: *mut EntryFrame) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the thunk passes its own leaked context and the frame on its stack.
        unsafe { enter(&*context, &mut *frame) }
    }));
    if outcome.is_err() {
        // Unwinding into the thunk's caller, which may be C, is undefined.
        std::process::abort();
    }
}

const THUNK_BYTES: usize = 32;
const THUNK_REGION_BYTES: usize = 1 << 20;

/// A region mapped twice from one memfd: written through one view, executed through the other,
/// so no page is ever writable and executable at once.
struct Thunks {
    write: usize,
    execute: usize,
    used: usize,
}

fn map_thunks() -> Option<Thunks> {
    // SAFETY: plain system calls on a new descriptor and new mappings; every failure is checked
    // and leaves nothing mapped that is used.
    unsafe {
        let fd = libc::memfd_create(c"uxdroid-thunks".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let mapped = libc::ftruncate(fd, THUNK_REGION_BYTES as libc::off_t) == 0;
        let map = |protection| {
            libc::mmap(
                std::ptr::null_mut(),
                THUNK_REGION_BYTES,
                protection,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        let write = if mapped {
            map(libc::PROT_READ | libc::PROT_WRITE)
        } else {
            libc::MAP_FAILED
        };
        let execute = if write != libc::MAP_FAILED {
            map(libc::PROT_READ | libc::PROT_EXEC)
        } else {
            libc::MAP_FAILED
        };
        libc::close(fd);
        if execute == libc::MAP_FAILED {
            return None;
        }
        Some(Thunks {
            write: write as usize,
            execute: execute as usize,
            used: 0,
        })
    }
}

/// A host function that calls `uxdroid_guest_entry` with `context` in rax.
pub(super) fn thunk(context: *const Callback) -> Option<usize> {
    static THUNKS: Mutex<Option<Option<Thunks>>> = Mutex::new(None);
    let mut thunks = THUNKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let thunks = thunks.get_or_insert_with(map_thunks).as_mut()?;
    if thunks.used + THUNK_BYTES > THUNK_REGION_BYTES {
        return None;
    }
    let mut code = [0xCCu8; THUNK_BYTES];
    // movabs rax, context
    code[0..2].copy_from_slice(&[0x48, 0xB8]);
    code[2..10].copy_from_slice(&(context as u64).to_le_bytes());
    // movabs r11, uxdroid_guest_entry
    code[10..12].copy_from_slice(&[0x49, 0xBB]);
    code[12..20].copy_from_slice(&(uxdroid_guest_entry as *const () as u64).to_le_bytes());
    // jmp r11
    code[20..23].copy_from_slice(&[0x41, 0xFF, 0xE3]);
    let offset = thunks.used;
    // SAFETY: the bytes lie in the write view, past every thunk handed out, so no thread runs
    // them yet; the shared mapping shows them in the execute view.
    unsafe {
        std::ptr::copy_nonoverlapping(
            code.as_ptr(),
            (thunks.write + offset) as *mut u8,
            THUNK_BYTES,
        );
    }
    thunks.used += THUNK_BYTES;
    Some(thunks.execute + offset)
}
