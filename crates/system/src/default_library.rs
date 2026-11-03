//! Default bodies as a shared library made at install time, so a start binds none of them.
//!
//! Every native of the install-time list becomes an exported function named the way JNI looks
//! natives up, `Java_<class>_<method>` or, for an overloaded method, with `__<arguments>` after it.
//! ART finds such a function only when a native is first called and nobody registered one, so the
//! natives that AOSP's host library and this one register never reach it, and the classes of the
//! others are not loaded at startup. The code is file pages, shared by every process that maps it.
//!
//! Each stub is the 32-byte body of [`crate::defaults`]: it passes its index to a hook and returns
//! zero, or what the hook returns for the bodies that make and free native objects. The three hooks
//! are a writable table in the library, pointing at a function that returns zero until
//! [`attach`] points them at this library's reporting and allocation functions.

use std::collections::{HashMap, HashSet};

use crate::defaults::{Body, Native, bodies};

/// The exported table: hooks to report, make a new object and get the free function, then the
/// number of bodies.
pub const HOOKS_SYMBOL: &str = "uxdroid_default_hooks";
const HOOKS: u64 = 3;
const TABLE: u64 = HOOKS + 1;
const STUB_SIZE: u64 = 32;
const PAGE: u64 = 0x1000;
const EHDR_SIZE: u64 = 64;
const PHDR_SIZE: u64 = 56;
const PHDRS: u64 = 4;
const SYM_SIZE: u64 = 24;
const RELA_SIZE: u64 = 24;
const DYN_SIZE: u64 = 16;
/// Returns zero in `rax` and `xmm0`: what a hook does before `attach`.
const FALLBACK: [u8; 7] = [0x31, 0xc0, 0x66, 0x0f, 0xef, 0xc0, 0xc3];
const FALLBACK_SIZE: u64 = 16;

/// JNI's escaping of a class, method or argument string: `/` separates, other characters that are
/// not ASCII letters or digits become `_1`, `_2`, `_3` or `_0xxxx` in UTF-16.
fn mangle(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match unit {
            0x2f => out.push('_'),
            0x5f => out.push_str("_1"),
            0x3b => out.push_str("_2"),
            0x5b => out.push_str("_3"),
            unit if unit < 0x80 && (unit as u8).is_ascii_alphanumeric() => {
                out.push(unit as u8 as char)
            }
            unit => out.push_str(&format!("_0{unit:04x}")),
        }
    }
    out
}

/// The name JNI looks up for each native: the short form, or the long one when the class has
/// another native of the same name. A name already given is `None`.
pub fn symbol_names(natives: &[Native]) -> Vec<Option<String>> {
    let mut signatures: HashMap<(&str, &str), HashSet<&str>> = HashMap::new();
    for native in natives {
        signatures
            .entry((&native.class, &native.name))
            .or_default()
            .insert(&native.signature);
    }
    let mut seen = HashSet::new();
    natives
        .iter()
        .map(|native| {
            let mut name = format!("Java_{}_{}", mangle(&native.class), mangle(&native.name));
            if signatures[&(native.class.as_str(), native.name.as_str())].len() > 1 {
                let arguments = native
                    .signature
                    .strip_prefix('(')
                    .and_then(|rest| rest.split(')').next())
                    .unwrap_or("");
                name = format!("{name}__{}", mangle(arguments));
            }
            seen.insert(name.clone()).then_some(name)
        })
        .collect()
}

/// The SysV ELF hash that `DT_HASH` tables use.
fn elf_hash(name: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &byte in name {
        hash = (hash << 4).wrapping_add(u32::from(byte));
        let high = hash & 0xf000_0000;
        if high != 0 {
            hash ^= high >> 24;
        }
        hash &= !high;
    }
    hash
}

fn align(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

struct Writer(Vec<u8>);

impl Writer {
    fn at(&mut self, offset: u64) {
        self.0.resize(offset as usize, 0);
    }
    fn u8s(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
    fn u16(&mut self, value: u16) {
        self.u8s(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.u8s(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.u8s(&value.to_le_bytes());
    }
    fn phdr(&mut self, kind: u32, flags: u32, offset: u64, size: u64, align: u64) {
        self.u32(kind);
        self.u32(flags);
        self.u64(offset);
        self.u64(offset);
        self.u64(offset);
        self.u64(size);
        self.u64(size);
        self.u64(align);
    }
}

/// `sub rsp,8; mov edi,index; call [rip+hook]; add rsp,8`, then zero in `rax` and `xmm0` or
/// what the hook returned, then `ret`.
fn stub(index: u32, address: u64, hook: u64, body: Body) -> [u8; STUB_SIZE as usize] {
    let mut code = [0xcc; STUB_SIZE as usize];
    code[..5].copy_from_slice(&[0x48, 0x83, 0xec, 0x08, 0xbf]);
    code[5..9].copy_from_slice(&index.to_le_bytes());
    code[9..11].copy_from_slice(&[0xff, 0x15]);
    let displacement = hook.wrapping_sub(address + 15) as u32;
    code[11..15].copy_from_slice(&displacement.to_le_bytes());
    code[15..19].copy_from_slice(&[0x48, 0x83, 0xc4, 0x08]);
    match body {
        Body::Zero => code[19..26].copy_from_slice(&FALLBACK),
        Body::Object | Body::Free => code[19] = 0xc3,
    }
    code
}

/// Builds the library for `natives`, in their order: stub `i` reports index `i`.
pub fn build(natives: &[Native]) -> Vec<u8> {
    let names = symbol_names(natives);
    // Symbol 0 is the null symbol; the hooks come last.
    let exported: Vec<(usize, &str)> = names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| Some((index, name.as_deref()?)))
        .collect();
    let symbols = exported.len() as u64 + 2;
    let mut strings = vec![0u8];
    let mut name_offsets = Vec::with_capacity(exported.len() + 1);
    for name in exported.iter().map(|(_, name)| *name).chain([HOOKS_SYMBOL]) {
        name_offsets.push(strings.len() as u32);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }

    let buckets = (symbols / 2).max(1);
    let hash = align(EHDR_SIZE + PHDRS * PHDR_SIZE, 8);
    let dynsym = align(hash + 8 + 4 * (buckets + symbols), 8);
    let dynstr = dynsym + SYM_SIZE * symbols;
    let rela = align(dynstr + strings.len() as u64, 8);
    let text = align(rela + RELA_SIZE * HOOKS, 16);
    let stubs = text + FALLBACK_SIZE;
    let text_end = stubs + STUB_SIZE * natives.len() as u64;
    let dynamic = align(text_end, PAGE);
    let dynamic_entries: [(u64, u64); 9] = [
        (4, hash),                  // DT_HASH
        (5, dynstr),                // DT_STRTAB
        (6, dynsym),                // DT_SYMTAB
        (10, strings.len() as u64), // DT_STRSZ
        (11, SYM_SIZE),             // DT_SYMENT
        (7, rela),                  // DT_RELA
        (8, RELA_SIZE * HOOKS),     // DT_RELASZ
        (9, RELA_SIZE),             // DT_RELAENT
        (0, 0),                     // DT_NULL
    ];
    let hooks = dynamic + DYN_SIZE * dynamic_entries.len() as u64;
    let end = hooks + 8 * TABLE;

    let mut out = Writer(Vec::with_capacity(end as usize));
    out.u8s(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.u16(3); // ET_DYN
    out.u16(62); // EM_X86_64
    out.u32(1);
    out.u64(0); // no entry point
    out.u64(EHDR_SIZE);
    out.u64(0); // no section headers
    out.u32(0);
    out.u16(EHDR_SIZE as u16);
    out.u16(PHDR_SIZE as u16);
    out.u16(PHDRS as u16);
    out.u16(64);
    out.u16(0);
    out.u16(0);
    const READ: u32 = 4;
    const WRITE: u32 = 2;
    const EXECUTE: u32 = 1;
    out.phdr(1, READ | EXECUTE, 0, text_end, PAGE); // PT_LOAD
    out.phdr(1, READ | WRITE, dynamic, end - dynamic, PAGE); // PT_LOAD
    out.phdr(2, READ | WRITE, dynamic, hooks - dynamic, 8); // PT_DYNAMIC
    // PT_GNU_STACK: without it, glibc would make the stack executable, or refuse the library.
    out.phdr(0x6474_e551, READ | WRITE, 0, 0, 16);

    out.at(hash);
    let symbol_names = std::iter::once(&[][..])
        .chain(exported.iter().map(|(_, name)| name.as_bytes()))
        .chain([HOOKS_SYMBOL.as_bytes()]);
    let mut heads = vec![0u32; buckets as usize];
    let mut chains = vec![0u32; symbols as usize];
    for (symbol, name) in symbol_names.enumerate().skip(1) {
        let bucket = (elf_hash(name) as u64 % buckets) as usize;
        chains[symbol] = heads[bucket];
        heads[bucket] = symbol as u32;
    }
    out.u32(buckets as u32);
    out.u32(symbols as u32);
    heads
        .iter()
        .chain(&chains)
        .for_each(|value| out.u32(*value));

    out.at(dynsym);
    out.u8s(&[0; SYM_SIZE as usize]);
    // Section index 1: any defined section; the library has no section headers to name it.
    let mut symbol = |name: u32, kind: u8, value: u64, size: u64| {
        out.u32(name);
        out.u8s(&[0x10 | kind, 0]); // STB_GLOBAL, default visibility
        out.u16(1);
        out.u64(value);
        out.u64(size);
    };
    for ((index, _), name) in exported.iter().zip(&name_offsets) {
        symbol(*name, 2, stubs + STUB_SIZE * *index as u64, STUB_SIZE); // STT_FUNC
    }
    symbol(name_offsets[exported.len()], 1, hooks, 8 * TABLE); // STT_OBJECT

    out.at(dynstr);
    out.u8s(&strings);
    out.at(rela);
    for hook in 0..HOOKS {
        out.u64(hooks + 8 * hook);
        out.u64(8); // R_X86_64_RELATIVE
        out.u64(text);
    }

    out.at(text);
    out.u8s(&FALLBACK);
    out.at(stubs);
    for (index, body) in bodies(natives).into_iter().enumerate() {
        let address = stubs + STUB_SIZE * index as u64;
        let hook = hooks
            + 8 * match body {
                Body::Zero => 0,
                Body::Object => 1,
                Body::Free => 2,
            };
        out.u8s(&stub(index as u32, address, hook, body));
    }

    out.at(dynamic);
    for (tag, value) in dynamic_entries {
        out.u64(tag);
        out.u64(value);
    }
    out.at(hooks + 8 * HOOKS);
    out.u64(natives.len() as u64);
    out.0
}

/// Points the hooks of the library at `library`, which `System.load` already loaded, at the
/// reporting and allocation functions, and names its natives from `list` on their first call.
/// Returns how many bodies the library holds.
#[cfg(target_arch = "x86_64")]
pub fn attach(library: &str, list: &str) -> Result<usize, String> {
    let path = std::ffi::CString::new(library).map_err(|error| error.to_string())?;
    // SAFETY: RTLD_NOLOAD only finds a library already loaded, which runs nothing.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
    if handle.is_null() {
        return Err(format!("{library} is not loaded"));
    }
    let symbol = std::ffi::CString::new(HOOKS_SYMBOL).unwrap_or_default();
    // SAFETY: the handle is open; dlclose only drops the reference dlopen added.
    let hooks = unsafe {
        let hooks = libc::dlsym(handle, symbol.as_ptr()) as *mut usize;
        libc::dlclose(handle);
        hooks
    };
    if hooks.is_null() {
        return Err(format!("{library} has no {HOOKS_SYMBOL}"));
    }
    crate::defaults::name_from(list);
    // SAFETY: the symbol is the library's table of three hooks, in a writable segment, and it stays
    // loaded: System.load never unloads.
    unsafe {
        for (slot, function) in crate::defaults::hook_functions().into_iter().enumerate() {
            std::ptr::write_volatile(hooks.add(slot), function);
        }
        Ok(hooks.add(HOOKS as usize).read())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::parse;

    #[test]
    fn names_follow_jni_short_and_long_forms() {
        let natives = parse(
            "android/os/Foo$Bar n_1 (I[Ljava/lang/String;)J\n\
             android/os/Foo$Bar n_1 (J)J\n\
             android/os/Foo$Bar single ()V\n\
             android/os/Foo$Bar single ()V\n\
             a/\u{e9}t\u{e9} x ()V\n",
        );
        assert_eq!(
            symbol_names(&natives),
            vec![
                Some("Java_android_os_Foo_00024Bar_n_11__I_3Ljava_lang_String_2".into()),
                Some("Java_android_os_Foo_00024Bar_n_11__J".into()),
                Some("Java_android_os_Foo_00024Bar_single".into()),
                None,
                Some("Java_a__000e9t_000e9_x".into()),
            ]
        );
    }

    #[test]
    fn hashes_like_the_elf_specification() {
        assert_eq!(elf_hash(b""), 0);
        assert_eq!(elf_hash(b"printf"), 0x077905a6);
        assert_eq!(elf_hash(b"exit"), 0x0006cf04);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_library_loads_and_its_bodies_return_zero_objects_and_free_functions() {
        let natives = parse(
            "a/B one (I)I\na/B two (D)D\na/B two (J)D\na/C nativeCreate (I)J\na/C nativeGetFinalizer ()J\n",
        );
        let directory = std::env::temp_dir().join(format!("defaults-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("libdefaults.so");
        std::fs::write(&path, build(&natives)).unwrap();
        let list = directory.join("natives.txt");
        std::fs::write(&list, "a/B one (I)I\n").unwrap();
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: the library has no initializers, and its functions take any arguments.
        unsafe {
            let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW);
            assert!(
                !handle.is_null(),
                "{:?}",
                std::ffi::CStr::from_ptr(libc::dlerror())
            );
            let find = |symbol: &str| {
                let symbol = std::ffi::CString::new(symbol).unwrap();
                libc::dlsym(handle, symbol.as_ptr()) as usize
            };
            // Before attach, every body returns zero.
            let create: extern "C" fn(i32) -> usize =
                std::mem::transmute(find("Java_a_C_nativeCreate"));
            assert_eq!(create(1), 0);

            assert_eq!(
                attach(path.to_str().unwrap(), list.to_str().unwrap()),
                Ok(5)
            );
            let one: extern "C" fn(i64, i64) -> i64 = std::mem::transmute(find("Java_a_B_one"));
            assert_eq!(one(7, 9), 0);
            let two: extern "C" fn(f64) -> f64 = std::mem::transmute(find("Java_a_B_two__D"));
            assert_eq!(two(2.5), 0.0);
            assert_ne!(find("Java_a_B_two__J"), 0);
            assert_eq!(find("Java_a_B_two"), 0);
            let finalizer: extern "C" fn() -> usize =
                std::mem::transmute(find("Java_a_C_nativeGetFinalizer"));
            let (first, second) = (create(1), create(2));
            assert!(first != 0 && second != 0 && first != second);
            let free: extern "C" fn(usize) = std::mem::transmute(finalizer());
            free(first);
            free(second);
        }
        let _ = std::fs::remove_dir_all(&directory);
    }
}
