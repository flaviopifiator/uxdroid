//! Default bodies for every framework native nobody implements.
//!
//! The list comes from install time (`runtime/framework/natives_list.py`). Each method gets a
//! 32-byte stub that reports its first call and returns zero in both `rax` and `xmm0`, which is the
//! zero, `false` or `null` of any return type. Arguments are ignored and the caller cleans up, so
//! one shape fits every signature. Only host ART calls these, never an app's own native code.
//!
//! One pattern needs more than zero: a class that keeps a native object hands its address to
//! `NativeAllocationRegistry` with the address of a function that frees it, and the registry refuses
//! a null object. In a class with a native returning that free function, the natives that create
//! objects get a stub returning a fresh small allocation, and the free function frees it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// One native method from the install-time list.
#[derive(Debug, PartialEq, Eq)]
pub struct Native {
    pub class: String,
    pub name: String,
    pub signature: String,
}

/// Parses lines of `<class> <method> <descriptor>`, skipping malformed ones.
pub fn parse(list: &str) -> Vec<Native> {
    list.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (class, name, signature) = (fields.next()?, fields.next()?, fields.next()?);
            signature.starts_with('(').then(|| Native {
                class: class.to_string(),
                name: name.to_string(),
                signature: signature.to_string(),
            })
        })
        .collect()
}

/// What a default body returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// Zero, `false` or `null`.
    Zero,
    /// A fresh native object, for natives that create one.
    Object,
    /// The function that frees those objects.
    Free,
}

fn is_free_getter(native: &Native) -> bool {
    let name = native.name.to_ascii_lowercase();
    native.signature == "()J"
        && ["finalizer", "delete", "destroy", "free"]
            .iter()
            .any(|w| name.contains(w))
}

/// A native returning a handle whose name starts, after a `native` or `n` prefix, with a verb that
/// makes one: `nativeCreate`, `nInit`, `native_new_instance`. A name that only holds such a word,
/// as `nativeGetNewestTimestamp` or `nRecreateCache`, returns a value, not a new object.
fn creates_object(native: &Native) -> bool {
    let name = native.name.as_str();
    let rest = name
        .strip_prefix("native")
        .or_else(|| {
            name.strip_prefix('n')
                .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
        })
        .unwrap_or(name)
        .trim_start_matches('_')
        .to_ascii_lowercase();
    native.signature.ends_with(")J")
        && ["init", "create", "new", "make"]
            .iter()
            .any(|verb| rest.starts_with(verb))
}

/// Chooses each native's body, in order.
pub fn bodies(natives: &[Native]) -> Vec<Body> {
    let owners: std::collections::HashSet<&str> = natives
        .iter()
        .filter(|n| is_free_getter(n))
        .map(|n| n.class.as_str())
        .collect();
    natives
        .iter()
        .map(|native| {
            if !owners.contains(native.class.as_str()) {
                Body::Zero
            } else if is_free_getter(native) {
                Body::Free
            } else if creates_object(native) {
                Body::Object
            } else {
                Body::Zero
            }
        })
        .collect()
}

struct Reported {
    names: Vec<String>,
    seen: Vec<AtomicBool>,
}

impl Reported {
    fn new(natives: &[Native]) -> Self {
        Reported {
            names: natives
                .iter()
                .map(|n| format!("{}.{}{}", n.class.replace('/', "."), n.name, n.signature))
                .collect(),
            seen: natives.iter().map(|_| AtomicBool::new(false)).collect(),
        }
    }
}

/// Names for the stubs made here, at startup.
static REPORTED: OnceLock<Reported> = OnceLock::new();
/// The list an install-time library of default bodies was made from, read on its first call.
static LIST: OnceLock<String> = OnceLock::new();
static LISTED: OnceLock<Reported> = OnceLock::new();

/// Names the natives of an install-time library from `list` when one is first called.
pub fn name_from(list: &str) {
    let _ = LIST.set(list.to_string());
}

fn reported() -> Option<&'static Reported> {
    REPORTED.get().or_else(|| {
        let list = LIST.get()?;
        Some(LISTED.get_or_init(|| {
            Reported::new(&parse(&std::fs::read_to_string(list).unwrap_or_default()))
        }))
    })
}

/// The functions an install-time library's hooks call, in its order: report, new object, free
/// function.
pub fn hook_functions() -> [usize; 3] {
    [
        first_call as extern "C" fn(u32) as usize,
        new_object as extern "C" fn(u32) -> usize as usize,
        free_function as extern "C" fn(u32) -> usize as usize,
    ]
}

/// Called by a stub with its method's index.
extern "C" fn first_call(index: u32) {
    let Some(reported) = reported() else {
        return;
    };
    let index = index as usize;
    if let (Some(seen), Some(name)) = (reported.seen.get(index), reported.names.get(index))
        && !seen.swap(true, Ordering::Relaxed)
    {
        // One write, so a line other threads print at the same time cannot land inside it.
        let line = format!("uxdroid: unimplemented native {name} returned its default\n");
        uxdroid_bridge::output::write_all(uxdroid_bridge::output::stderr_fd(), line.as_bytes());
    }
}

/// Reports the first call, then returns a new object: 16 zeroed bytes, freed by [`free_object`].
extern "C" fn new_object(index: u32) -> usize {
    first_call(index);
    Box::into_raw(Box::new([0u64; 2])) as usize
}

/// Frees an object from [`new_object`]. `NativeAllocationRegistry` calls it when the Java side dies.
extern "C" fn free_object(object: *mut [u64; 2]) {
    if !object.is_null() {
        // SAFETY: the registry passes only addresses that new_object returned, once each.
        drop(unsafe { Box::from_raw(object) });
    }
}

/// Reports the first call, then returns the address of [`free_object`].
extern "C" fn free_function(index: u32) -> usize {
    first_call(index);
    free_object as extern "C" fn(*mut [u64; 2]) as usize
}

#[cfg(target_arch = "x86_64")]
const STUB_SIZE: usize = 32;
#[cfg(target_arch = "aarch64")]
const STUB_SIZE: usize = 48;

/// `sub rsp,8; mov edi,index; movabs rax,first_call; call rax; add rsp,8; xor eax,eax; pxor xmm0,xmm0; ret`
#[cfg(target_arch = "x86_64")]
fn stub(index: u32, target: u64) -> [u8; STUB_SIZE] {
    let mut code = [0u8; STUB_SIZE];
    code[..5].copy_from_slice(&[0x48, 0x83, 0xec, 0x08, 0xbf]);
    code[5..9].copy_from_slice(&index.to_le_bytes());
    code[9..11].copy_from_slice(&[0x48, 0xb8]);
    code[11..19].copy_from_slice(&target.to_le_bytes());
    code[19..].copy_from_slice(&[
        0xff, 0xd0, 0x48, 0x83, 0xc4, 0x08, 0x31, 0xc0, 0x66, 0x0f, 0xef, 0xc0, 0xc3,
    ]);
    code
}

/// `sub rsp,8; mov edi,index; movabs rax,target; call rax; add rsp,8; ret`: returns what target returns.
#[cfg(target_arch = "x86_64")]
fn forwarding_stub(index: u32, target: u64) -> [u8; STUB_SIZE] {
    let mut code = [0xcc; STUB_SIZE];
    code[..5].copy_from_slice(&[0x48, 0x83, 0xec, 0x08, 0xbf]);
    code[5..9].copy_from_slice(&index.to_le_bytes());
    code[9..11].copy_from_slice(&[0x48, 0xb8]);
    code[11..19].copy_from_slice(&target.to_le_bytes());
    code[19..26].copy_from_slice(&[0xff, 0xd0, 0x48, 0x83, 0xc4, 0x08, 0xc3]);
    code
}

/// AArch64 instructions the stubs are made of.
#[cfg(target_arch = "aarch64")]
pub(crate) mod a64 {
    /// `stp x29, x30, [sp, #-16]!`
    pub const PUSH_FRAME: u32 = 0xa9bf_7bfd;
    /// `ldp x29, x30, [sp], #16`
    pub const POP_FRAME: u32 = 0xa8c1_7bfd;
    /// `blr x16`
    pub const CALL_X16: u32 = 0xd63f_0200;
    /// `mov x0, xzr`
    pub const ZERO_X0: u32 = 0xaa1f_03e0;
    /// `fmov d0, xzr`
    pub const ZERO_D0: u32 = 0x9e67_03e0;
    /// `ret`
    pub const RET: u32 = 0xd65f_03c0;
    /// `brk #0`, filling space nothing branches to.
    pub const BREAK: u32 = 0xd420_0000;

    /// `movz w0, #low` and `movk w0, #high, lsl #16`: the 32-bit `value` in the first argument.
    pub fn move_w0(value: u32) -> [u32; 2] {
        [
            0x5280_0000 | ((value & 0xffff) << 5),
            0x72a0_0000 | ((value >> 16) << 5),
        ]
    }

    /// `ldr x16, <literal>`, with the literal `offset` bytes after the instruction.
    pub fn load_x16_literal(offset: u32) -> u32 {
        0x5800_0000 | ((offset / 4) << 5) | 16
    }

    /// `adrp x16, <page>`, the 4 KiB page at `target` as seen from the instruction at `address`.
    pub fn page_address_x16(address: u64, target: u64) -> u32 {
        let pages = ((target >> 12) as i64).wrapping_sub((address >> 12) as i64) as u32;
        0x9000_0000 | ((pages & 3) << 29) | (((pages >> 2) & 0x7_ffff) << 5) | 16
    }

    /// `ldr x16, [x16, #offset]`, with `offset` a multiple of 8 below 32 KiB.
    pub fn load_x16_from_x16(offset: u32) -> u32 {
        0xf940_0000 | ((offset / 8) << 10) | (16 << 5) | 16
    }

    /// `b <target>` from the instruction at `address`, within 128 MiB.
    pub fn branch(address: u64, target: u64) -> u32 {
        0x1400_0000 | ((target.wrapping_sub(address) as u32 / 4) & 0x3ff_ffff)
    }

    pub fn bytes<const N: usize>(words: &[u32]) -> [u8; N] {
        let mut code = [0; N];
        for (chunk, word) in code.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        code
    }
}

/// Calls `first_call(index)`, then returns zero in `x0` and `d0`. `target` is a literal at 40.
#[cfg(target_arch = "aarch64")]
fn stub(index: u32, target: u64) -> [u8; STUB_SIZE] {
    use a64::*;
    let [low, high] = move_w0(index);
    let mut code: [u8; STUB_SIZE] = bytes(&[
        PUSH_FRAME,
        low,
        high,
        load_x16_literal(40 - 12),
        CALL_X16,
        POP_FRAME,
        ZERO_X0,
        ZERO_D0,
        RET,
        BREAK,
    ]);
    code[40..].copy_from_slice(&target.to_le_bytes());
    code
}

/// Calls `target(index)` and returns what it returns.
#[cfg(target_arch = "aarch64")]
fn forwarding_stub(index: u32, target: u64) -> [u8; STUB_SIZE] {
    use a64::*;
    let [low, high] = move_w0(index);
    let mut code: [u8; STUB_SIZE] = bytes(&[
        PUSH_FRAME,
        low,
        high,
        load_x16_literal(40 - 12),
        CALL_X16,
        POP_FRAME,
        RET,
        BREAK,
        BREAK,
        BREAK,
    ]);
    code[40..].copy_from_slice(&target.to_le_bytes());
    code
}

/// Writes one stub per method into executable memory and returns their addresses, in order.
/// The memory lives as long as the process. Call once.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn stubs(natives: &[Native]) -> Result<Vec<usize>, String> {
    REPORTED
        .set(Reported::new(natives))
        .map_err(|_| "default bodies were already made".to_string())?;
    if natives.is_empty() {
        return Ok(Vec::new());
    }
    let size = natives.len() * STUB_SIZE;
    // SAFETY: a fresh anonymous mapping, written before it becomes executable and never unmapped.
    unsafe {
        let memory = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if memory == libc::MAP_FAILED {
            return Err(format!("mmap: {}", std::io::Error::last_os_error()));
        }
        let base = memory as *mut u8;
        let report = first_call as extern "C" fn(u32) as usize as u64;
        let object = new_object as extern "C" fn(u32) -> usize as usize as u64;
        let free = free_function as extern "C" fn(u32) -> usize as usize as u64;
        for (index, body) in bodies(natives).into_iter().enumerate() {
            let code = match body {
                Body::Zero => stub(index as u32, report),
                Body::Object => forwarding_stub(index as u32, object),
                Body::Free => forwarding_stub(index as u32, free),
            };
            std::ptr::copy_nonoverlapping(code.as_ptr(), base.add(index * STUB_SIZE), STUB_SIZE);
        }
        // AArch64's instruction cache does not see data writes until they are cleaned to it.
        #[cfg(target_arch = "aarch64")]
        {
            unsafe extern "C" {
                fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
            }
            __clear_cache(base.cast(), base.add(size).cast());
        }
        if libc::mprotect(memory, size, libc::PROT_READ | libc::PROT_EXEC) != 0 {
            return Err(format!("mprotect: {}", std::io::Error::last_os_error()));
        }
        Ok((0..natives.len())
            .map(|index| base as usize + index * STUB_SIZE)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_install_time_list() {
        let list = "android/os/Trace nativeIsTagEnabled (J)Z\nbroken line\nandroid/os/Foo$Bar names (I)[Ljava/lang/String;\n";
        assert_eq!(
            parse(list),
            vec![
                Native {
                    class: "android/os/Trace".into(),
                    name: "nativeIsTagEnabled".into(),
                    signature: "(J)Z".into()
                },
                Native {
                    class: "android/os/Foo$Bar".into(),
                    name: "names".into(),
                    signature: "(I)[Ljava/lang/String;".into(),
                },
            ]
        );
    }

    #[test]
    fn only_classes_that_free_their_objects_get_objects() {
        let natives = parse(
            "android/os/PerfettoTrace$Category native_init (Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)J\n\
             android/os/PerfettoTrace$Category native_delete ()J\n\
             android/os/PerfettoTrace$Category native_is_enabled (J)Z\n\
             android/os/PerfettoTrace$Category native_get_extra_ptr (J)J\n\
             a/Plain nativeCreate ()J\n\
             a/Plain nativeDelete (J)V\n",
        );
        assert_eq!(
            bodies(&natives),
            vec![
                Body::Object,
                Body::Free,
                Body::Zero,
                Body::Zero,
                Body::Zero,
                Body::Zero
            ]
        );
    }

    #[test]
    fn only_a_name_that_starts_with_making_something_makes_an_object() {
        let natives = parse(
            "a/C nativeGetFinalizer ()J\n\
             a/C nativeCreate (I)J\n\
             a/C nInit ()J\n\
             a/C native_new_instance ()J\n\
             a/C makeNative ()J\n\
             a/C nativeGetNewestTimestamp (J)J\n\
             a/C nativeGetInitialValue (J)J\n\
             a/C nRecreateCache (J)J\n",
        );
        assert_eq!(
            bodies(&natives),
            vec![
                Body::Free,
                Body::Object,
                Body::Object,
                Body::Object,
                Body::Object,
                Body::Zero,
                Body::Zero,
                Body::Zero
            ]
        );
    }

    #[test]
    fn stubs_return_zero_for_every_kind_and_report_first_calls() {
        let natives = parse(
            "a/B one ()I\na/B two (JLjava/lang/String;)D\na/B three ()Ljava/lang/Object;\n\
             a/C nativeCreate (I)J\na/C nativeGetFinalizer ()J\n",
        );
        let addresses = stubs(&natives).unwrap();
        // SAFETY: each stub takes and ignores any arguments, and returns zero in rax and xmm0.
        unsafe {
            let int: extern "C" fn(i64, i64) -> i64 = std::mem::transmute(addresses[0]);
            let float: extern "C" fn(f64) -> f64 = std::mem::transmute(addresses[1]);
            let object: extern "C" fn() -> *const u8 = std::mem::transmute(addresses[2]);
            assert_eq!(int(7, 9), 0);
            assert_eq!(float(2.5), 0.0);
            assert!(object().is_null());
            assert_eq!(int(1, 2), 0);

            // An object is never null nor shared, and the free function takes it back.
            let create: extern "C" fn(i32) -> usize = std::mem::transmute(addresses[3]);
            let finalizer: extern "C" fn() -> usize = std::mem::transmute(addresses[4]);
            let (first, second) = (create(1), create(2));
            assert!(first != 0 && second != 0 && first != second);
            let free: extern "C" fn(usize) = std::mem::transmute(finalizer());
            free(first);
            free(second);
        }
        let reported = REPORTED.get().unwrap();
        assert!(
            reported
                .seen
                .iter()
                .all(|seen| seen.load(Ordering::Relaxed))
        );
        assert_eq!(reported.names[1], "a.B.two(JLjava/lang/String;)D");
    }
}
