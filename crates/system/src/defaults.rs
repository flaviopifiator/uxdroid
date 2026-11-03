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

fn creates_object(native: &Native) -> bool {
    let name = native.name.to_ascii_lowercase();
    native.signature.ends_with(")J")
        && ["init", "create", "new", "make"]
            .iter()
            .any(|w| name.contains(w))
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
        eprintln!("uxdroid: unimplemented native {name} returned its default");
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

const STUB_SIZE: usize = 32;

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

/// Writes one stub per method into executable memory and returns their addresses, in order.
/// The memory lives as long as the process. Call once.
#[cfg(target_arch = "x86_64")]
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

    #[cfg(target_arch = "x86_64")]
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
