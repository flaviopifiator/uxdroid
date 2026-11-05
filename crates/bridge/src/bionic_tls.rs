//! bionic's thread pointer slots on an AArch64 host.
//!
//! arm64 code built for Android reads bionic's TLS slots at fixed offsets from `TPIDR_EL0`: the
//! stack protector loads its canary from slot 5, at `TPIDR_EL0 + 40`, in every protected function.
//! glibc's thread pointer has its 16-byte control block there and then the first module's TLS, so
//! that word would be whatever variable glibc put there, and any function that changed it would
//! fail its stack check. The executable reserves the words past glibc's control block, slots 2 to
//! 7, as the first 48 bytes of its own TLS, and hands [`reserve`] a function that returns their
//! address for the calling thread; the loader refuses host arm64 libraries until the reservation is
//! proven to sit exactly there.

use std::ffi::c_void;
use std::sync::OnceLock;

/// Where slot 2 lies from the thread pointer: past glibc's `dtv` and private word.
const SLOTS_OFFSET: usize = 16;
/// Slot 5, the stack guard, inside the reservation.
const STACK_GUARD: usize = 5 * 8 - SLOTS_OFFSET;
/// Slots 2 to 7.
pub const SLOTS_SIZE: usize = 48;

/// Returns the calling thread's reservation.
pub type SlotsAddress = unsafe extern "C" fn() -> usize;

static RESERVED: OnceLock<SlotsAddress> = OnceLock::new();

fn thread_pointer() -> usize {
    let pointer: usize;
    // SAFETY: reading TPIDR_EL0 has no side effects.
    unsafe { std::arch::asm!("mrs {}, tpidr_el0", out(reg) pointer, options(nomem, nostack)) };
    pointer
}

/// Whether `address` places the calling thread's reservation over bionic's slots.
fn check_with(address: SlotsAddress) -> Result<(), String> {
    // SAFETY: the function given to `reserve` takes nothing and only computes an address.
    let slots = unsafe { address() };
    let tp = thread_pointer();
    if slots.wrapping_sub(tp) != SLOTS_OFFSET {
        return Err(format!(
            "the TLS reserved for bionic's slots is at the thread pointer plus {:#x}, not {SLOTS_OFFSET:#x}",
            slots.wrapping_sub(tp)
        ));
    }
    Ok(())
}

/// Takes the executable's reservation, checks that it covers bionic's slots on this thread, and
/// gives slot 5 a random stack canary, in this thread and in the TLS image every later thread
/// starts from. Threads that already run keep the image's zero, a canary that never changes.
pub fn reserve(address: SlotsAddress) -> Result<(), String> {
    check_with(address)?;
    if RESERVED.set(address).is_err() {
        return Ok(());
    }
    let mut canary = [0u8; 8];
    // SAFETY: the buffer is 8 writable bytes.
    if unsafe { libc::getrandom(canary.as_mut_ptr().cast(), canary.len(), 0) } != 8 {
        return Err(format!("getrandom: {}", std::io::Error::last_os_error()));
    }
    let canary = u64::from_ne_bytes(canary);
    // SAFETY: the check proved the reservation is this thread's, SLOTS_SIZE bytes long.
    unsafe { ((thread_pointer() + SLOTS_OFFSET + STACK_GUARD) as *mut u64).write(canary) };
    let image = executable_tls_image().ok_or("the executable has no TLS image")?;
    write_protected((image + STACK_GUARD) as *mut u64, canary)
}

/// Whether host arm64 libraries may load: the reservation was made and covers this thread's slots.
pub(crate) fn check() -> Result<(), String> {
    let address = RESERVED
        .get()
        .ok_or("this process reserved no TLS for bionic's thread pointer slots")?;
    check_with(*address)
}

/// The executable's TLS initialization image, from its program headers.
fn executable_tls_image() -> Option<usize> {
    unsafe extern "C" fn first(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        found: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: glibc passes valid program headers; the executable comes first.
        unsafe {
            let info = &*info;
            let headers = std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum));
            if let Some(tls) = headers.iter().find(|header| header.p_type == libc::PT_TLS) {
                *found.cast::<usize>() = info.dlpi_addr as usize + tls.p_vaddr as usize;
            }
        }
        1
    }
    let mut image = 0usize;
    // SAFETY: the callback writes only `image`, and stops after the first object.
    unsafe { libc::dl_iterate_phdr(Some(first), (&raw mut image).cast()) };
    (image != 0).then_some(image)
}

/// Writes `value` to a word that may be on a read-only page, such as the TLS image after RELRO,
/// and puts the page's protection back.
fn write_protected(word: *mut u64, value: u64) -> Result<(), String> {
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let start = word as usize & !(page - 1);
    let protection = page_protection(start).ok_or("the TLS image is not mapped")?;
    // SAFETY: the page is the executable's own TLS image; it is made writable only for one aligned
    // word store, before any thread that could copy it is started, and its protection restored.
    unsafe {
        if libc::mprotect(start as *mut c_void, page, protection | libc::PROT_WRITE) != 0 {
            return Err(format!("mprotect: {}", std::io::Error::last_os_error()));
        }
        word.write_volatile(value);
        if libc::mprotect(start as *mut c_void, page, protection) != 0 {
            return Err(format!("mprotect: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// The protection of the mapping holding `address`, from `/proc/self/maps`.
fn page_protection(address: usize) -> Option<libc::c_int> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().find_map(|line| {
        let (range, rest) = line.split_once(' ')?;
        let (low, high) = range.split_once('-')?;
        let (low, high) = (
            usize::from_str_radix(low, 16).ok()?,
            usize::from_str_radix(high, 16).ok()?,
        );
        if !(low..high).contains(&address) {
            return None;
        }
        let flags = rest.as_bytes();
        let mut protection = libc::PROT_NONE;
        if flags.first() == Some(&b'r') {
            protection |= libc::PROT_READ;
        }
        if flags.get(1) == Some(&b'w') {
            protection |= libc::PROT_WRITE;
        }
        if flags.get(2) == Some(&b'x') {
            protection |= libc::PROT_EXEC;
        }
        Some(protection)
    })
}
