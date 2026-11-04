//! Finds, maps, links and initializes shared objects and their dependencies.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::elf::{
    Dynamic, EHDR_SIZE, ElfError, Machine, PHDR_SIZE, PT_DYNAMIC, PT_LOAD, PT_TLS, ProgramHeader,
    R_AARCH64_ABS64, R_AARCH64_GLOB_DAT, R_AARCH64_IRELATIVE, R_AARCH64_JUMP_SLOT,
    R_AARCH64_RELATIVE, R_AARCH64_TLS_DTPMOD64, R_AARCH64_TLS_DTPREL64, R_AARCH64_TLS_TPREL64,
    R_AARCH64_TLSDESC, R_X86_64_64, R_X86_64_DTPMOD64, R_X86_64_DTPOFF64, R_X86_64_GLOB_DAT,
    R_X86_64_IRELATIVE, R_X86_64_JUMP_SLOT, R_X86_64_RELATIVE, RELA_SIZE, Rela, STB_WEAK, STT_FUNC,
    STT_GNU_IFUNC, STT_TLS, SYM_SIZE, Symbol, for_each_packed, for_each_relr, gnu_hash,
    parse_dynamic_for, parse_header, parse_program_headers, sysv_hash,
};
use crate::guest;
use crate::image::Image;
use crate::shims;
use crate::table::{self, Table};

/// libraries that uxdroid provides from glibc and the shims instead of loading a file. The other
/// Android system libraries, such as `liblog.so` or `libandroid.so`, are signature tables.
const SYSTEM_LIBRARIES: &[&str] = &["libc.so", "libdl.so", "libm.so", "libstdc++.so"];
/// Stops a crafted dependency graph from exhausting memory or the stack.
const MAX_LIBRARIES_PER_LOAD: usize = 1024;
const MAX_REPORTED_SYMBOLS: usize = 4096;

#[derive(Debug)]
pub enum Error {
    Unreadable(PathBuf, std::io::Error),
    Invalid(String, ElfError),
    MissingLibrary {
        needed: String,
        by: String,
    },
    TooManyLibraries,
    /// Every library with symbols found nowhere, each with all of its names.
    UnresolvedSymbols(Vec<(String, Vec<String>)>),
    /// A library for a guest ABI, and why no translator can run it.
    NoTranslator(String, String),
    /// A library whose initializer stopped in guest code.
    GuestFault(String, String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unreadable(path, source) => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Error::Invalid(library, source) => {
                write!(f, "{}: {source}", library.escape_debug())
            }
            Error::MissingLibrary { needed, by } => write!(
                f,
                "{} needs {}, which was not found",
                by.escape_debug(),
                needed.escape_debug()
            ),
            Error::TooManyLibraries => write!(f, "too many dependencies"),
            Error::UnresolvedSymbols(libraries) => {
                let reports: Vec<String> = libraries
                    .iter()
                    .map(|(library, symbols)| {
                        let names: Vec<String> = symbols
                            .iter()
                            .map(|symbol| symbol.escape_debug().to_string())
                            .collect();
                        format!(
                            "{}: unresolved symbols: {}",
                            library.escape_debug(),
                            names.join(", ")
                        )
                    })
                    .collect();
                write!(f, "{}", reports.join("; "))
            }
            Error::NoTranslator(library, reason) => write!(
                f,
                "{} is arm64-v8a code, which needs the translator: {reason}",
                library.escape_debug()
            ),
            Error::GuestFault(library, reason) => {
                write!(
                    f,
                    "{}: guest code stopped: {reason}",
                    library.escape_debug()
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// A shared object in memory. Libraries stay loaded until the process exits.
#[derive(Debug)]
pub struct Library {
    name: String,
    machine: Machine,
    path: CString,
    image: Image,
    headers: Vec<ProgramHeader>,
    /// A copy of the program headers for `dl_iterate_phdr`; words keep them aligned.
    raw_headers: Box<[u64]>,
    dynamic: Dynamic,
    dependencies: OnceLock<Vec<&'static Library>>,
    /// The signature tables among this library's dependencies, in `DT_NEEDED` order.
    tables: OnceLock<Vec<&'static Table>>,
}

impl Library {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The CPU the library's code is for.
    pub fn machine(&self) -> Machine {
        self.machine
    }

    /// The names of the libraries this one directly depends on, loaded or provided.
    pub fn needed(&self) -> Vec<String> {
        self.needed_names().unwrap_or_default()
    }

    fn needed_names(&self) -> Result<Vec<String>, ElfError> {
        self.dynamic
            .needed
            .iter()
            .map(|&offset| {
                let name = self
                    .image
                    .string(self.dynamic.strtab, self.dynamic.strsz, offset)?;
                Ok(String::from_utf8_lossy(name).into_owned())
            })
            .collect()
    }

    /// The address of an exported symbol, after running its resolver if it is an ifunc.
    pub fn symbol(&self, name: &str) -> Option<*const c_void> {
        let symbol = self.find(name.as_bytes())?;
        let address = self.symbol_address(&symbol).ok()?;
        Some(address as usize as *const c_void)
    }

    /// The runtime address of one of this library's defined symbols.
    fn symbol_address(&self, symbol: &Symbol) -> Result<u64, ElfError> {
        let address = self.image.bias().wrapping_add(symbol.value as usize) as u64;
        match symbol.kind {
            STT_TLS => Err(ElfError::Unsupported("ELF TLS")),
            STT_GNU_IFUNC if self.machine != Machine::X86_64 => {
                Err(ElfError::Unsupported("ifunc in guest code"))
            }
            STT_GNU_IFUNC => {
                if !self.image.is_executable(symbol.value) {
                    return Err(ElfError::Malformed(
                        "ifunc resolver outside executable segments",
                    ));
                }
                // SAFETY: the check above proves the resolver is in this library's code, and
                // the ELF format defines its signature.
                Ok(unsafe { call_resolver(address) })
            }
            _ => Ok(address),
        }
    }

    /// Calls an exported `int name(void)`.
    ///
    /// # Safety
    /// The symbol must be a function with that signature.
    pub unsafe fn call_i32(&self, name: &str) -> Option<i32> {
        let address = self.symbol(name)?;
        if self.machine == Machine::Aarch64 {
            return match guest::call_i32(address as u64) {
                Ok(value) => Some(value),
                Err(reason) => crate::shims::die(format_args!(
                    "uxdroid: {name} stopped in guest code: {reason}\n"
                )),
            };
        }
        // SAFETY: the caller promises the signature.
        let function: unsafe extern "C" fn() -> c_int = unsafe { std::mem::transmute(address) };
        // SAFETY: as above.
        Some(unsafe { function() })
    }

    fn find(&self, name: &[u8]) -> Option<Symbol> {
        if self.dynamic.gnu_hash != 0 {
            self.find_gnu(name).ok().flatten()
        } else if self.dynamic.hash != 0 {
            self.find_sysv(name).ok().flatten()
        } else {
            None
        }
    }

    fn u32_at(&self, vaddr: u64) -> Result<u32, ElfError> {
        let mut word = [0; 4];
        word.copy_from_slice(self.image.bytes(vaddr, 4)?);
        Ok(u32::from_le_bytes(word))
    }

    fn symbol_at(&self, index: u32) -> Result<Symbol, ElfError> {
        let vaddr = u64::from(index)
            .checked_mul(SYM_SIZE as u64)
            .and_then(|offset| offset.checked_add(self.dynamic.symtab))
            .ok_or(ElfError::Malformed("symbol index overflows"))?;
        Symbol::parse(self.image.bytes(vaddr, SYM_SIZE as u64)?)
    }

    fn symbol_name(&self, symbol: &Symbol) -> Result<&[u8], ElfError> {
        self.image.string(
            self.dynamic.strtab,
            self.dynamic.strsz,
            u64::from(symbol.name),
        )
    }

    fn matches(&self, index: u32, name: &[u8]) -> Result<Option<Symbol>, ElfError> {
        let symbol = self.symbol_at(index)?;
        Ok((symbol.is_defined() && self.symbol_name(&symbol)? == name).then_some(symbol))
    }

    fn find_gnu(&self, name: &[u8]) -> Result<Option<Symbol>, ElfError> {
        const BLOOM_WORD_BITS: u32 = 64;

        let table = self.dynamic.gnu_hash;
        let field = |index: u64| self.u32_at(table.wrapping_add(index * 4));
        let bucket_count = field(0)?;
        let symbol_offset = field(1)?;
        let bloom_size = field(2)?;
        let bloom_shift = field(3)?;
        if bucket_count == 0 || bloom_size == 0 {
            return Ok(None);
        }

        let hash = gnu_hash(name);
        let bloom_start = table.wrapping_add(16);
        let bloom_index = (hash / BLOOM_WORD_BITS) % bloom_size;
        let bloom = self
            .image
            .read_u64(bloom_start.wrapping_add(u64::from(bloom_index) * 8))?;
        let first_bit = hash % BLOOM_WORD_BITS;
        let second_bit = hash.checked_shr(bloom_shift).unwrap_or(0) % BLOOM_WORD_BITS;
        if bloom & (1 << first_bit) == 0 || bloom & (1 << second_bit) == 0 {
            return Ok(None);
        }

        let buckets = bloom_start.wrapping_add(u64::from(bloom_size) * 8);
        let chains = buckets.wrapping_add(u64::from(bucket_count) * 4);
        let mut index = self.u32_at(buckets.wrapping_add(u64::from(hash % bucket_count) * 4))?;
        if index < symbol_offset {
            return Ok(None);
        }
        loop {
            let chain = self.u32_at(chains.wrapping_add(u64::from(index - symbol_offset) * 4))?;
            if chain | 1 == hash | 1
                && let Some(symbol) = self.matches(index, name)?
            {
                return Ok(Some(symbol));
            }
            if chain & 1 != 0 {
                return Ok(None);
            }
            index = index
                .checked_add(1)
                .ok_or(ElfError::Malformed("unterminated hash chain"))?;
        }
    }

    fn find_sysv(&self, name: &[u8]) -> Result<Option<Symbol>, ElfError> {
        let table = self.dynamic.hash;
        let bucket_count = self.u32_at(table)?;
        let chain_count = self.u32_at(table.wrapping_add(4))?;
        if bucket_count == 0 {
            return Ok(None);
        }
        let buckets = table.wrapping_add(8);
        let chains = buckets.wrapping_add(u64::from(bucket_count) * 4);
        // The table must really hold the chains it declares, which bounds the walk below.
        self.image.bytes(chains, u64::from(chain_count) * 4)?;
        let bucket = u64::from(sysv_hash(name) % bucket_count);
        let mut index = self.u32_at(buckets.wrapping_add(bucket * 4))?;
        // A chain visits each symbol at most once; more steps mean a crafted cycle.
        for _ in 0..=chain_count {
            if index == 0 {
                return Ok(None);
            }
            if let Some(symbol) = self.matches(index, name)? {
                return Ok(Some(symbol));
            }
            index = self.u32_at(chains.wrapping_add(u64::from(index) * 4))?;
        }
        Err(ElfError::Malformed("cycle in hash chain"))
    }

    fn deps(&self) -> &[&'static Library] {
        self.dependencies.get().map_or(&[], Vec::as_slice)
    }

    fn invalid(&self, error: ElfError) -> Error {
        Error::Invalid(self.name.clone(), error)
    }

    fn init_functions(&self) -> Result<Vec<u64>, ElfError> {
        const SKIPPED_ENTRY: u64 = u64::MAX;

        let mut functions = Vec::new();
        if self.dynamic.init != 0 {
            functions.push(self.image.bias().wrapping_add(self.dynamic.init as usize) as u64);
        }
        if self.dynamic.init_array != 0 {
            let table = self
                .image
                .bytes(self.dynamic.init_array, self.dynamic.init_arraysz)?;
            for raw in table.as_chunks::<8>().0 {
                let mut word = [0; 8];
                word.copy_from_slice(raw);
                let address = u64::from_le_bytes(word);
                if address != 0 && address != SKIPPED_ENTRY {
                    functions.push(address);
                }
            }
        }
        for &address in &functions {
            let vaddr = (address as usize).wrapping_sub(self.image.bias()) as u64;
            if !self.image.is_executable(vaddr) {
                return Err(ElfError::Malformed(
                    "initializer outside executable segments",
                ));
            }
        }
        Ok(functions)
    }
}

#[derive(Default)]
struct Registry {
    libraries: Vec<&'static Library>,
    search_dirs: Vec<PathBuf>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

struct Locked(MutexGuard<'static, Option<Registry>>);

impl Locked {
    fn get(&mut self) -> &mut Registry {
        self.0.get_or_insert_with(Registry::default)
    }
}

fn lock() -> Locked {
    // Poisoning would only mean a panic elsewhere; the list itself stays consistent.
    Locked(
        REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

fn snapshot() -> Vec<&'static Library> {
    lock().get().libraries.clone()
}

/// Loads a library and its dependencies, then runs their initializers.
///
/// A library's own directory and `search_dirs` are searched for its dependencies. Initializers
/// are the library's own code: a crafted library can do anything from there.
pub fn load(path: &Path, search_dirs: &[PathBuf]) -> Result<&'static Library, Error> {
    shims::use_bionic_default_locale();
    let (root, new_libraries) = {
        let mut guard = lock();
        let registry = guard.get();
        for dir in search_dirs {
            if !registry.search_dirs.contains(dir) {
                registry.search_dirs.push(dir.clone());
            }
        }
        let own_dir = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        if !registry.search_dirs.iter().any(|dir| dir == own_dir) {
            registry.search_dirs.push(own_dir.to_path_buf());
        }
        link_tree(path, registry)?
    };

    run_initializers(&new_libraries)?;
    Ok(root)
}

/// Maps the whole tree and links it, leaving the registry untouched on failure.
fn link_tree(
    path: &Path,
    registry: &mut Registry,
) -> Result<(&'static Library, Vec<&'static Library>), Error> {
    let root_name = file_name(path);
    let same_file = |library: &Library| {
        let loaded = Path::new(std::ffi::OsStr::from_bytes(library.path.as_bytes()));
        match (std::fs::canonicalize(path), std::fs::canonicalize(loaded)) {
            (Ok(requested), Ok(loaded)) => requested == loaded,
            _ => false,
        }
    };
    if let Some(existing) = registry
        .libraries
        .iter()
        .find(|library| library.name == root_name && same_file(library))
    {
        return Ok((existing, Vec::new()));
    }

    let mut pending: Vec<&'static Library> = Vec::new();
    match map_and_link(path, registry, &mut pending) {
        Ok(ordered) => {
            registry.libraries.extend(pending.iter().copied());
            Ok((pending[0], ordered))
        }
        Err(error) => {
            for library in &pending {
                library.image.unmap();
            }
            Err(error)
        }
    }
}

fn map_and_link(
    path: &Path,
    registry: &Registry,
    pending: &mut Vec<&'static Library>,
) -> Result<Vec<&'static Library>, Error> {
    pending.push(map_library(path)?);

    // Breadth-first, matching the order in which the Android linker searches symbols.
    let mut next = 0;
    while next < pending.len() {
        let library = pending[next];
        next += 1;
        let needed = library
            .needed_names()
            .map_err(|error| library.invalid(error))?;
        let mut dependencies = Vec::new();
        let mut tables = Vec::new();
        for name in needed {
            if SYSTEM_LIBRARIES.contains(&name.as_str()) {
                continue;
            }
            // Android's own libraries come from the host even when an app ships a file of that name.
            if let Some(table) = table::find(&name) {
                tables.push(table);
                continue;
            }
            let known = registry
                .libraries
                .iter()
                .chain(pending.iter())
                .find(|candidate| candidate.name == name)
                .copied();
            let dependency = match known {
                Some(dependency) => dependency,
                None => {
                    if pending.len() >= MAX_LIBRARIES_PER_LOAD {
                        return Err(Error::TooManyLibraries);
                    }
                    let path =
                        find_dependency(&name, registry).ok_or_else(|| Error::MissingLibrary {
                            needed: name.clone(),
                            by: library.name.clone(),
                        })?;
                    let dependency = map_library(&path)?;
                    pending.push(dependency);
                    dependency
                }
            };
            if dependency.machine != library.machine {
                return Err(
                    dependency.invalid(ElfError::Unsupported("a dependency for another ABI"))
                );
            }
            dependencies.push(dependency);
        }
        let _ = library.dependencies.set(dependencies);
        let _ = library.tables.set(tables);
    }

    let root = pending[0];
    if root.machine != Machine::X86_64 {
        guest::engine().map_err(|reason| Error::NoTranslator(root.name.clone(), reason))?;
        GUEST_LOADED.store(true, std::sync::atomic::Ordering::Release);
    }
    let scope = Scope::of(root);
    let ordered = dependencies_first(root, pending);
    let mut unresolved = Vec::new();
    for library in &ordered {
        let mut missing = BTreeSet::new();
        relocate(library, &scope, &mut missing).map_err(|error| library.invalid(error))?;
        if !missing.is_empty() {
            unresolved.push((library.name.clone(), missing.into_iter().collect()));
        }
    }
    if !unresolved.is_empty() {
        return Err(Error::UnresolvedSymbols(unresolved));
    }
    for library in &ordered {
        library
            .image
            .protect_relro(&library.headers)
            .map_err(|error| library.invalid(error))?;
    }
    Ok(ordered)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
}

fn find_dependency(name: &str, registry: &Registry) -> Option<PathBuf> {
    // A name with a directory could escape the search path.
    if name.contains('/') {
        return None;
    }
    registry
        .search_dirs
        .iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn map_library(path: &Path) -> Result<&'static Library, Error> {
    let unreadable = |source| Error::Unreadable(path.to_path_buf(), source);
    let name = file_name(path);
    let invalid = |error| Error::Invalid(name.clone(), error);

    // Checked first because opening a FIFO would block forever.
    let metadata = std::fs::metadata(path).map_err(unreadable)?;
    if !metadata.is_file() {
        return Err(invalid(ElfError::Malformed("not a regular file")));
    }
    let file = File::open(path).map_err(unreadable)?;
    let file_len = metadata.len();

    let mut header = [0; EHDR_SIZE];
    if file_len < EHDR_SIZE as u64 {
        return Err(invalid(ElfError::Malformed("truncated header")));
    }
    file.read_exact_at(&mut header, 0).map_err(unreadable)?;
    let table = parse_header(&header).map_err(invalid)?;
    let machine = table.machine;

    let table_len = (table.count * PHDR_SIZE) as u64;
    if table.offset.saturating_add(table_len) > file_len {
        return Err(invalid(ElfError::Malformed(
            "program headers extend past end of file",
        )));
    }
    let mut raw_table = vec![0; table_len as usize];
    file.read_exact_at(&mut raw_table, table.offset)
        .map_err(unreadable)?;
    let headers = parse_program_headers(&raw_table).map_err(invalid)?;

    if headers.iter().any(|header| header.kind == PT_TLS) {
        return Err(invalid(ElfError::Unsupported("ELF TLS")));
    }
    let dynamic_header = headers
        .iter()
        .find(|header| header.kind == PT_DYNAMIC)
        .ok_or_else(|| invalid(ElfError::Malformed("no dynamic section")))?;

    let image = Image::map(&file, file_len, &headers).map_err(invalid)?;
    let dynamic = image
        .bytes(dynamic_header.vaddr, dynamic_header.filesz)
        .and_then(|section| parse_dynamic_for(section, machine));
    let dynamic = match dynamic {
        Ok(dynamic) => dynamic,
        Err(error) => {
            image.unmap();
            return Err(invalid(error));
        }
    };

    let raw_headers: Box<[u64]> = raw_table
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&word| u64::from_le_bytes(word))
        .collect();
    let path = CString::new(path.as_os_str().as_bytes()).unwrap_or_default();
    Ok(Box::leak(Box::new(Library {
        name,
        machine,
        path,
        image,
        headers,
        raw_headers,
        dynamic,
        dependencies: OnceLock::new(),
        tables: OnceLock::new(),
    })))
}

/// Where a library's symbols are searched: loaded libraries first, then the tables they need.
struct Scope {
    libraries: Vec<&'static Library>,
    tables: Vec<&'static Table>,
}

impl Scope {
    fn of(root: &'static Library) -> Scope {
        let libraries = breadth_first(root);
        let mut tables: Vec<&'static Table> = Vec::new();
        for library in &libraries {
            for &needed in library.tables.get().map_or(&[][..], Vec::as_slice) {
                if !tables.iter().any(|seen| std::ptr::eq(*seen, needed)) {
                    tables.push(needed);
                }
            }
        }
        // On Android, app_process links liblog, which makes its symbols global to every library.
        let liblog = table::liblog();
        if !tables.iter().any(|seen| std::ptr::eq(*seen, liblog)) {
            tables.push(liblog);
        }
        Scope { libraries, tables }
    }
}

fn breadth_first(root: &'static Library) -> Vec<&'static Library> {
    let mut order = vec![root];
    let mut next = 0;
    while next < order.len() {
        for &dependency in order[next].deps() {
            if !order.iter().any(|seen| std::ptr::eq(*seen, dependency)) {
                order.push(dependency);
            }
        }
        next += 1;
    }
    order
}

/// The new libraries with each one after its dependencies, cycles broken arbitrarily.
fn dependencies_first(
    root: &'static Library,
    new_libraries: &[&'static Library],
) -> Vec<&'static Library> {
    fn visit(
        library: &'static Library,
        new_libraries: &[&'static Library],
        seen: &mut Vec<*const Library>,
        order: &mut Vec<&'static Library>,
    ) {
        if seen.contains(&std::ptr::from_ref(library))
            || !new_libraries.iter().any(|new| std::ptr::eq(*new, library))
        {
            return;
        }
        seen.push(library);
        for &dependency in library.deps() {
            visit(dependency, new_libraries, seen, order);
        }
        order.push(library);
    }

    let mut seen = Vec::new();
    let mut order = Vec::new();
    visit(root, new_libraries, &mut seen, &mut order);
    order
}

/// A symbol's address, searching `scope` in order, then the shims and glibc. Guest code reaches
/// the host only through the guest frame's stubs.
fn resolve(
    scope: &Scope,
    name: &[u8],
    is_data: bool,
    machine: Machine,
) -> Result<Option<u64>, ElfError> {
    for library in &scope.libraries {
        if let Some(symbol) = library.find(name) {
            return library.symbol_address(&symbol).map(Some);
        }
    }
    if machine != Machine::X86_64 {
        return guest::import(name, &scope.tables, is_data);
    }
    for table in &scope.tables {
        if let Some(export) = table.find(name) {
            // A host library may lack an export, which then counts as missing.
            return Ok(export.bind().map(|address| address as u64));
        }
    }
    let mut owned = Vec::with_capacity(name.len() + 1);
    owned.extend_from_slice(name);
    owned.push(0);
    let Ok(name) = CStr::from_bytes_with_nul(&owned) else {
        return Ok(None);
    };
    Ok(shims::resolve(name, is_data).map(|address| address as u64))
}

/// # Safety
/// `address` must be an ifunc resolver in a mapped, executable segment.
unsafe fn call_resolver(address: u64) -> u64 {
    // SAFETY: the caller promises the address and its signature.
    unsafe {
        let resolver: unsafe extern "C" fn() -> u64 = std::mem::transmute(address as usize);
        resolver()
    }
}

fn relocate(
    library: &'static Library,
    scope: &Scope,
    missing: &mut BTreeSet<String>,
) -> Result<(), ElfError> {
    let image = &library.image;
    let dynamic = &library.dynamic;
    let bias = image.bias() as u64;

    if dynamic.relrsz != 0 {
        let table = image.bytes(dynamic.relr, dynamic.relrsz)?;
        for_each_relr(table, |vaddr| {
            image.write_u64(vaddr, image.read_u64(vaddr)?.wrapping_add(bias))
        })?;
    }

    let mut cache = HashMap::new();
    let mut ifuncs = Vec::new();
    let mut apply =
        |rela: Rela| apply_relocation(library, scope, rela, &mut cache, &mut ifuncs, missing);

    if dynamic.android_relasz != 0 {
        let span = library
            .headers
            .iter()
            .filter(|header| header.kind == PT_LOAD)
            .map(|header| header.end())
            .max()
            .unwrap_or(0);
        let data = image.bytes(dynamic.android_rela, dynamic.android_relasz)?;
        for_each_packed(data, span / 8, &mut apply)?;
    }
    for (table, size) in [
        (dynamic.rela, dynamic.relasz),
        (dynamic.jmprel, dynamic.pltrelsz),
    ] {
        if size != 0 {
            for raw in image.bytes(table, size)?.as_chunks::<RELA_SIZE>().0 {
                apply(Rela::parse(raw))?;
            }
        }
    }

    // After everything else, as resolvers may read relocated data.
    for (vaddr, resolver) in ifuncs {
        if !image.is_executable(resolver) {
            return Err(ElfError::Malformed(
                "ifunc resolver outside executable segments",
            ));
        }
        // SAFETY: the check above proves the resolver is in this library's code.
        let target = unsafe { call_resolver(bias.wrapping_add(resolver)) };
        image.write_u64(vaddr, target)?;
    }
    Ok(())
}

fn apply_relocation(
    library: &'static Library,
    scope: &Scope,
    rela: Rela,
    cache: &mut HashMap<u32, u64>,
    ifuncs: &mut Vec<(u64, u64)>,
    missing: &mut BTreeSet<String>,
) -> Result<(), ElfError> {
    let image = &library.image;
    let bias = image.bias() as u64;
    match Relocation::of(library.machine, rela.kind) {
        Relocation::Relative => image.write_u64(rela.offset, bias.wrapping_add_signed(rela.addend)),
        Relocation::IRelative if library.machine != Machine::X86_64 => {
            Err(ElfError::Unsupported("ifunc in guest code"))
        }
        Relocation::IRelative => {
            ifuncs.push((rela.offset, rela.addend as u64));
            Ok(())
        }
        kind @ (Relocation::Absolute | Relocation::Slot) => {
            let address = match cache.get(&rela.symbol) {
                Some(&address) => address,
                None => {
                    let address = symbol_address(library, scope, rela, missing)?;
                    cache.insert(rela.symbol, address);
                    address
                }
            };
            // AArch64's GLOB_DAT and JUMP_SLOT take the addend too; it is zero in practice.
            let value = if kind == Relocation::Absolute || library.machine == Machine::Aarch64 {
                address.wrapping_add_signed(rela.addend)
            } else {
                address
            };
            image.write_u64(rela.offset, value)
        }
        Relocation::Tls => Err(ElfError::Unsupported("ELF TLS")),
        Relocation::Other => Err(ElfError::Unsupported("relocation type")),
    }
}

/// What a relocation does, whatever the machine's number for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relocation {
    Relative,
    IRelative,
    /// The symbol's address plus the addend.
    Absolute,
    /// The symbol's address, for the GOT or a PLT slot.
    Slot,
    Tls,
    Other,
}

impl Relocation {
    fn of(machine: Machine, kind: u32) -> Relocation {
        match (machine, kind) {
            (Machine::X86_64, R_X86_64_RELATIVE) | (Machine::Aarch64, R_AARCH64_RELATIVE) => {
                Relocation::Relative
            }
            (Machine::X86_64, R_X86_64_IRELATIVE) | (Machine::Aarch64, R_AARCH64_IRELATIVE) => {
                Relocation::IRelative
            }
            (Machine::X86_64, R_X86_64_64) | (Machine::Aarch64, R_AARCH64_ABS64) => {
                Relocation::Absolute
            }
            (Machine::X86_64, R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT)
            | (Machine::Aarch64, R_AARCH64_GLOB_DAT | R_AARCH64_JUMP_SLOT) => Relocation::Slot,
            (Machine::X86_64, R_X86_64_DTPMOD64 | R_X86_64_DTPOFF64)
            | (
                Machine::Aarch64,
                R_AARCH64_TLS_DTPMOD64
                | R_AARCH64_TLS_DTPREL64
                | R_AARCH64_TLS_TPREL64
                | R_AARCH64_TLSDESC,
            ) => Relocation::Tls,
            _ => Relocation::Other,
        }
    }

    fn is_jump_slot(machine: Machine, kind: u32) -> bool {
        matches!(
            (machine, kind),
            (Machine::X86_64, R_X86_64_JUMP_SLOT) | (Machine::Aarch64, R_AARCH64_JUMP_SLOT)
        )
    }
}

fn symbol_address(
    library: &'static Library,
    scope: &Scope,
    rela: Rela,
    missing: &mut BTreeSet<String>,
) -> Result<u64, ElfError> {
    let index = rela.symbol;
    if index == 0 {
        return Ok(0);
    }
    let symbol = library.symbol_at(index)?;
    let name = library.symbol_name(&symbol)?;
    // Only something known to be a function can trap when called; reading a trap as data can't.
    let is_data = symbol.kind != STT_FUNC && !Relocation::is_jump_slot(library.machine, rela.kind);
    if let Some(address) = resolve(scope, name, is_data, library.machine)? {
        return Ok(address);
    }
    // The limit bounds the memory of an error report for a crafted file.
    if symbol.bind != STB_WEAK && missing.len() < MAX_REPORTED_SYMBOLS {
        missing.insert(String::from_utf8_lossy(name).into_owned());
    }
    Ok(0)
}

fn run_initializers(libraries: &[&'static Library]) -> Result<(), Error> {
    unsafe extern "C" {
        static environ: *const *const c_char;
    }
    let no_arguments: [*const c_char; 1] = [std::ptr::null()];
    for library in libraries {
        let functions = library
            .init_functions()
            .map_err(|error| library.invalid(error))?;
        if library.machine == Machine::Aarch64 {
            for address in functions {
                // SAFETY: reads glibc's own pointer.
                let environment = unsafe { environ } as u64;
                guest::call_initializer(address, environment)
                    .map_err(|reason| Error::GuestFault(library.name.clone(), reason))?;
            }
            continue;
        }
        for address in functions {
            // SAFETY: `init_functions` proved the address is in this library's code, and the
            // ELF format defines this signature. `environ` is glibc's own pointer.
            unsafe {
                let function: unsafe extern "C" fn(
                    c_int,
                    *const *const c_char,
                    *const *const c_char,
                ) = std::mem::transmute(address as usize);
                function(0, no_arguments.as_ptr(), environ);
            }
        }
    }
    Ok(())
}

// --- The libdl functions that libraries call ---

/// Handle of the libraries that have no file of their own.
const SYSTEM_HANDLE: *mut c_void = std::ptr::dangling_mut::<u8>().cast();

thread_local! {
    static DL_ERROR: RefCell<(Option<CString>, Option<CString>)> =
        const { RefCell::new((None, None)) };
}

fn set_dl_error(message: String) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    DL_ERROR.with(|state| state.borrow_mut().0 = Some(text));
}

pub unsafe extern "C" fn dlopen(name: *const c_char, _flags: c_int) -> *mut c_void {
    if name.is_null() {
        return SYSTEM_HANDLE;
    }
    // SAFETY: the libdl contract makes `name` a C string.
    let name = unsafe { CStr::from_ptr(name) };
    let path = Path::new(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let plain_name = !name.to_bytes().contains(&b'/');

    if plain_name && SYSTEM_LIBRARIES.contains(&path.to_string_lossy().as_ref()) {
        return SYSTEM_HANDLE;
    }
    if plain_name && let Some(table) = table::find(&path.to_string_lossy()) {
        return std::ptr::from_ref(table).cast_mut().cast();
    }
    let target = if plain_name {
        let mut guard = lock();
        let registry = guard.get();
        let text = path.to_string_lossy().into_owned();
        if let Some(library) = registry
            .libraries
            .iter()
            .find(|library| library.name == text)
        {
            return std::ptr::from_ref(*library).cast_mut().cast();
        }
        find_dependency(&text, registry)
    } else {
        Some(path.to_path_buf())
    };
    let Some(target) = target else {
        set_dl_error(format!("library {} not found", name.to_string_lossy()));
        return std::ptr::null_mut();
    };
    match load(&target, &[]) {
        Ok(library) => std::ptr::from_ref(library).cast_mut().cast(),
        Err(error) => {
            set_dl_error(error.to_string());
            std::ptr::null_mut()
        }
    }
}

pub unsafe extern "C" fn android_dlopen_ext(
    name: *const c_char,
    flags: c_int,
    _info: *const c_void,
) -> *mut c_void {
    // SAFETY: forwards the caller's arguments under the same contract.
    unsafe { dlopen(name, flags) }
}

pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

    // SAFETY: the libdl contract makes `name` a C string.
    let symbol = unsafe { CStr::from_ptr(name) };
    let libraries = snapshot();
    let scope = if handle == RTLD_DEFAULT {
        Scope {
            libraries,
            tables: table::all(),
        }
    } else if let Some(library) = libraries
        .iter()
        .find(|library| std::ptr::from_ref(**library).cast::<c_void>() == handle.cast_const())
    {
        Scope::of(library)
    } else if let Some(table) = table::from_handle(handle) {
        Scope {
            libraries: Vec::new(),
            tables: vec![table],
        }
    } else {
        // SYSTEM_HANDLE or RTLD_NEXT: only the host remains to search.
        Scope {
            libraries: Vec::new(),
            tables: Vec::new(),
        }
    };
    match resolve(&scope, symbol.to_bytes(), false, Machine::X86_64) {
        Ok(Some(address)) => address as *mut c_void,
        _ => {
            set_dl_error(format!("undefined symbol: {}", symbol.to_string_lossy()));
            std::ptr::null_mut()
        }
    }
}

pub extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    // Libraries stay loaded, which Android also allows and which keeps atexit handlers valid.
    0
}

pub extern "C" fn dlerror() -> *const c_char {
    DL_ERROR.with(|state| {
        let mut state = state.borrow_mut();
        // The returned text must stay valid until the next call, so it moves to the second slot.
        state.1 = state.0.take();
        state
            .1
            .as_ref()
            .map_or(std::ptr::null(), |text| text.as_ptr())
    })
}

pub unsafe extern "C" fn dladdr(address: *const c_void, info: *mut libc::Dl_info) -> c_int {
    let owner = snapshot()
        .into_iter()
        .find(|library| library.image.contains_address(address as usize));
    match owner {
        Some(library) => {
            // SAFETY: the libdl contract makes `info` writable.
            unsafe {
                *info = libc::Dl_info {
                    dli_fname: library.path.as_ptr(),
                    dli_fbase: library.image.bias() as *mut c_void,
                    dli_sname: std::ptr::null(),
                    dli_saddr: std::ptr::null_mut(),
                };
            }
            1
        }
        // SAFETY: forwards the caller's arguments under the same contract.
        None => unsafe { libc::dladdr(address, info) },
    }
}

/// Reports loaded libraries before the host's. C++ unwinders find exception tables through this.
pub unsafe extern "C" fn dl_iterate_phdr(
    callback: Option<unsafe extern "C" fn(*mut libc::dl_phdr_info, usize, *mut c_void) -> c_int>,
    data: *mut c_void,
) -> c_int {
    let Some(function) = callback else {
        return 0;
    };
    for library in snapshot() {
        // SAFETY: zero is valid for this plain C struct.
        let mut info: libc::dl_phdr_info = unsafe { std::mem::zeroed() };
        info.dlpi_addr = library.image.bias() as u64;
        info.dlpi_name = library.path.as_ptr();
        info.dlpi_phdr = library.raw_headers.as_ptr().cast();
        info.dlpi_phnum = (library.raw_headers.len() * 8 / PHDR_SIZE) as u16;
        // SAFETY: `info` is fully initialized and outlives the call; the headers are immutable.
        let status = unsafe { function(&mut info, size_of::<libc::dl_phdr_info>(), data) };
        if status != 0 {
            return status;
        }
    }
    // SAFETY: forwards the caller's callback under the same contract.
    unsafe { libc::dl_iterate_phdr(callback, data) }
}

/// `library+offset` for an address in a loaded library, or the bare address.
///
/// Used on the way to stopping the program, possibly from a thread that holds the registry, so it
/// never waits for the lock: a busy registry gives the bare address.
pub(crate) fn describe_address(address: u64) -> String {
    let libraries = match REGISTRY.try_lock() {
        Ok(mut guard) => guard
            .get_or_insert_with(Registry::default)
            .libraries
            .clone(),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned
            .into_inner()
            .get_or_insert_with(Registry::default)
            .libraries
            .clone(),
        Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
    };
    let owner = libraries
        .into_iter()
        .find(|library| library.image.contains_address(address as usize));
    match owner {
        Some(library) => format!(
            "{}+{:#x}",
            library.name.escape_debug(),
            (address as usize).wrapping_sub(library.image.bias())
        ),
        None => format!("{address:#x}"),
    }
}

/// Set once any guest library is mapped, so host-only processes answer `is_guest_address` at once.
static GUEST_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether `address` is in a loaded library built for a guest ABI.
pub fn is_guest_address(address: usize) -> bool {
    if !GUEST_LOADED.load(std::sync::atomic::Ordering::Acquire) {
        return false;
    }
    snapshot().into_iter().any(|library| {
        library.machine != Machine::X86_64 && library.image.contains_address(address)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describing_an_address_never_waits_for_the_registry() {
        let held = lock();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(describe_address(0x1234));
        });
        let described = receiver.recv_timeout(std::time::Duration::from_secs(5));
        drop(held);
        assert_eq!(described.as_deref(), Ok("0x1234"));
    }
}
