//! Default bodies for every framework native nobody implements.
//!
//! The list comes from install time (`runtime/framework/natives_list.py`). Each method gets a
//! 32-byte stub that reports its first call and returns zero in both `rax` and `xmm0`, which is the
//! zero, `false` or `null` of any return type. Arguments are ignored and the caller cleans up, so
//! one shape fits every signature. Only host ART calls these, never an app's own native code.

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

struct Reported {
    names: Vec<String>,
    seen: Vec<AtomicBool>,
}

static REPORTED: OnceLock<Reported> = OnceLock::new();

/// Called by a stub with its method's index.
extern "C" fn first_call(index: u32) {
    let Some(reported) = REPORTED.get() else {
        return;
    };
    let index = index as usize;
    if let (Some(seen), Some(name)) = (reported.seen.get(index), reported.names.get(index))
        && !seen.swap(true, Ordering::Relaxed)
    {
        eprintln!("uxdroid: unimplemented native {name} returned its default");
    }
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

/// Writes one stub per method into executable memory and returns their addresses, in order.
/// The memory lives as long as the process. Call once.
#[cfg(target_arch = "x86_64")]
pub fn stubs(natives: &[Native]) -> Result<Vec<usize>, String> {
    let names = natives
        .iter()
        .map(|n| format!("{}.{}{}", n.class.replace('/', "."), n.name, n.signature))
        .collect();
    let seen = natives.iter().map(|_| AtomicBool::new(false)).collect();
    REPORTED
        .set(Reported { names, seen })
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
        let target = first_call as extern "C" fn(u32) as usize as u64;
        for index in 0..natives.len() {
            let code = stub(index as u32, target);
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

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stubs_return_zero_for_every_kind_and_report_first_calls() {
        let natives =
            parse("a/B one ()I\na/B two (JLjava/lang/String;)D\na/B three ()Ljava/lang/Object;\n");
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
