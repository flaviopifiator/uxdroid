//! Install-time work for an app's guest libraries: every import of every library is resolved
//! against the app's own libraries, the signature tables and the guest libc, and each library's
//! JNI entry points and function boundaries are recorded, all from the files' bytes, without
//! mapping or running them. The files are untrusted: every offset is checked and every count
//! bounded.

use crate::elf::{
    ElfError, Machine, PHDR_SIZE, PT_DYNAMIC, PT_LOAD, ProgramHeader, SYM_SIZE, parse_dynamic_for,
    parse_header, parse_program_headers,
};
use crate::table::Table;

const STB_WEAK: u8 = 2;
const STT_FUNC: u8 = 2;
const STT_GNU_IFUNC: u8 = 10;
const STT_OBJECT: u8 = 1;
const STT_TLS: u8 = 6;
/// Far above any real library's dynamic symbol count, so a crafted hash table cannot make the
/// reader walk or allocate much.
const MAX_SYMBOLS: usize = 1 << 20;
const MAX_NAME_BYTES: usize = 4096;

/// A dynamic symbol of a guest library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub defined: bool,
    pub weak: bool,
    /// A function or ifunc; an undefined import often has no type at all.
    pub function: bool,
    /// A data object, which binds to an address rather than a stub.
    pub object: bool,
    pub value: u64,
    pub size: u64,
}

/// What a guest library's file says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestLibrary {
    pub name: String,
    pub needed: Vec<String>,
    pub symbols: Vec<Symbol>,
}

/// How an import binds when the library loads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    /// Defined by a library of the app, named.
    App(String),
    /// A function or object of a signature table, by its library.
    Table(&'static str),
    /// The guest libc: a signature, a hand-written call or an object.
    Libc,
    /// The host has it but guest code cannot call it yet: it binds to a stub that stops the
    /// program naming it if called.
    Stub(&'static str),
    /// A function on the official list of this NDK library, which uxdroid does not have yet: it
    /// binds to a stub that stops the program naming the library and the function if called.
    Unimplemented(&'static str),
    /// Found nowhere. A weak import is null; any other fails the load.
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub name: String,
    pub weak: bool,
    pub binding: Binding,
}

/// One library's install-time record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryPlan {
    pub name: String,
    pub needed: Vec<String>,
    pub imports: Vec<Import>,
    /// `JNI_OnLoad`, `JNI_OnUnload` and the `Java_` natives ART finds by name.
    pub jni: Vec<String>,
    /// Exported functions' start and size, where translation can begin.
    pub functions: Vec<(u64, u64)>,
}

fn bytes_at(file: &[u8], offset: u64, length: u64) -> Result<&[u8], ElfError> {
    let start = usize::try_from(offset).map_err(|_| ElfError::Malformed("offset out of range"))?;
    let length = usize::try_from(length).map_err(|_| ElfError::Malformed("size out of range"))?;
    let end = start
        .checked_add(length)
        .ok_or(ElfError::Malformed("offset out of range"))?;
    file.get(start..end)
        .ok_or(ElfError::Malformed("extends past end of file"))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The file offset of virtual address `vaddr`, through the loadable segments.
fn file_offset(headers: &[ProgramHeader], vaddr: u64) -> Result<u64, ElfError> {
    headers
        .iter()
        .filter(|header| header.kind == PT_LOAD)
        .find(|header| vaddr >= header.vaddr && vaddr - header.vaddr < header.filesz)
        .and_then(|header| header.offset.checked_add(vaddr - header.vaddr))
        .ok_or(ElfError::Malformed("address outside the file's segments"))
}

/// The rest of the file from `vaddr` on.
fn from_vaddr<'a>(
    file: &'a [u8],
    headers: &[ProgramHeader],
    vaddr: u64,
) -> Result<&'a [u8], ElfError> {
    let offset = usize::try_from(file_offset(headers, vaddr)?)
        .map_err(|_| ElfError::Malformed("offset out of range"))?;
    file.get(offset..)
        .ok_or(ElfError::Malformed("extends past end of file"))
}

fn string(strtab: &[u8], offset: u64) -> Result<String, ElfError> {
    let start = usize::try_from(offset).map_err(|_| ElfError::Malformed("name out of range"))?;
    let rest = strtab
        .get(start..)
        .ok_or(ElfError::Malformed("name out of range"))?;
    let end = rest
        .iter()
        .take(MAX_NAME_BYTES)
        .position(|&byte| byte == 0)
        .ok_or(ElfError::Malformed("unterminated name"))?;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// How many dynamic symbols there are: DT_HASH's chain count, or the end of DT_GNU_HASH's last
/// chain.
fn symbol_count(
    file: &[u8],
    headers: &[ProgramHeader],
    hash: u64,
    gnu_hash: u64,
) -> Result<usize, ElfError> {
    let malformed = ElfError::Malformed("bad hash table");
    if hash != 0 {
        let table = from_vaddr(file, headers, hash)?;
        return Ok(u32_at(table, 4).ok_or(malformed)? as usize);
    }
    if gnu_hash == 0 {
        return Ok(0);
    }
    let table = from_vaddr(file, headers, gnu_hash)?;
    let buckets = u32_at(table, 0).ok_or(ElfError::Malformed("bad hash table"))? as usize;
    let first = u32_at(table, 4).ok_or(ElfError::Malformed("bad hash table"))? as usize;
    let blooms = u32_at(table, 8).ok_or(ElfError::Malformed("bad hash table"))? as usize;
    if buckets > MAX_SYMBOLS || blooms > MAX_SYMBOLS {
        return Err(malformed);
    }
    let buckets_at = 16 + 8 * blooms;
    let chains_at = buckets_at + 4 * buckets;
    let mut last = 0usize;
    for bucket in 0..buckets {
        let start =
            u32_at(table, buckets_at + 4 * bucket).ok_or(ElfError::Malformed("bad hash table"))?;
        last = last.max(start as usize);
    }
    if last < first {
        return Ok(first);
    }
    while last < MAX_SYMBOLS {
        let chain = u32_at(table, chains_at + 4 * (last - first))
            .ok_or(ElfError::Malformed("bad hash table"))?;
        last += 1;
        if chain & 1 != 0 {
            return Ok(last);
        }
    }
    Err(malformed)
}

/// Reads an arm64 library's needed libraries and dynamic symbols from its file.
pub fn read(name: &str, file: &[u8]) -> Result<GuestLibrary, ElfError> {
    let table = parse_header(file)?;
    if table.machine != Machine::Aarch64 {
        return Err(ElfError::Unsupported("not an arm64 library"));
    }
    let headers = parse_program_headers(bytes_at(
        file,
        table.offset,
        (table.count * PHDR_SIZE) as u64,
    )?)?;
    let dynamic_header = headers
        .iter()
        .find(|header| header.kind == PT_DYNAMIC)
        .ok_or(ElfError::Malformed("no dynamic section"))?;
    let dynamic = parse_dynamic_for(
        bytes_at(file, dynamic_header.offset, dynamic_header.filesz)?,
        Machine::Aarch64,
    )?;
    let strtab = from_vaddr(file, &headers, dynamic.strtab)?;
    let strtab = strtab
        .get(
            ..usize::try_from(dynamic.strsz)
                .unwrap_or(usize::MAX)
                .min(strtab.len()),
        )
        .unwrap_or(strtab);
    let needed = dynamic
        .needed
        .iter()
        .map(|&offset| string(strtab, offset))
        .collect::<Result<_, _>>()?;

    let count = symbol_count(file, &headers, dynamic.hash, dynamic.gnu_hash)?;
    if count > MAX_SYMBOLS {
        return Err(ElfError::Malformed("too many symbols"));
    }
    let symtab = if count == 0 {
        &[][..]
    } else {
        from_vaddr(file, &headers, dynamic.symtab)?
    };
    let mut symbols = Vec::new();
    for index in 1..count {
        let entry = symtab
            .get(index * SYM_SIZE..(index + 1) * SYM_SIZE)
            .ok_or(ElfError::Malformed("symbol table past end of file"))?;
        let info = entry[4];
        let (binding, kind) = (info >> 4, info & 0xF);
        if binding == 0 {
            continue; // local
        }
        let name_offset = u32_at(entry, 0).unwrap_or(0);
        let shndx = u16::from_le_bytes([entry[6], entry[7]]);
        symbols.push(Symbol {
            name: string(strtab, u64::from(name_offset))?,
            defined: shndx != 0,
            weak: binding == STB_WEAK,
            function: kind == STT_FUNC || kind == STT_GNU_IFUNC,
            object: kind == STT_OBJECT || kind == STT_TLS,
            value: u64_at(entry, 8).unwrap_or(0),
            size: u64_at(entry, 16).unwrap_or(0),
        });
    }
    Ok(GuestLibrary {
        name: name.to_string(),
        needed,
        symbols,
    })
}

/// Whether the loader provides `name` itself, as libc, a signature table, liblog or an NDK library
/// whose functions all stop the program naming them, so an app need not ship it.
pub fn provided(name: &str, tables: &[&'static Table]) -> bool {
    crate::loader::is_system_library(name)
        || name == crate::table::liblog().library
        || tables.iter().any(|table| table.library == name)
        || crate::unimplemented::is_ndk_library(name)
}

/// The libraries `root` needs, by name among `libraries`, breadth first as the loader searches.
fn search_order<'a>(
    root: &'a GuestLibrary,
    libraries: &'a [GuestLibrary],
) -> Vec<&'a GuestLibrary> {
    let mut order = vec![root];
    let mut next = 0;
    while next < order.len() {
        for needed in &order[next].needed {
            if let Some(library) = libraries.iter().find(|library| &library.name == needed)
                && !order.iter().any(|seen| std::ptr::eq(*seen, library))
            {
                order.push(library);
            }
        }
        next += 1;
    }
    order
}

/// Resolves every import of every library as a start would, and records what a start needs.
pub fn plan(libraries: &[GuestLibrary], tables: &[&'static Table]) -> Vec<LibraryPlan> {
    libraries
        .iter()
        .map(|library| {
            let order = search_order(library, libraries);
            // As the loader does: the tables the libraries need, and liblog, which app_process
            // makes global on Android.
            let needed_tables: Vec<&'static Table> = tables
                .iter()
                .copied()
                .filter(|table| {
                    order
                        .iter()
                        .any(|lib| lib.needed.iter().any(|n| n == table.library))
                })
                .chain(std::iter::once(crate::table::liblog()))
                .collect();
            // The NDK libraries the loader serves rather than the app's files, as it searches them.
            let needed_ndk: Vec<&str> = order
                .iter()
                .flat_map(|lib| &lib.needed)
                .map(String::as_str)
                .filter(|name| !libraries.iter().any(|library| library.name == *name))
                .collect();
            let imports = library
                .symbols
                .iter()
                .filter(|symbol| !symbol.defined && !symbol.name.is_empty())
                .map(|symbol| {
                    let owner = order.iter().find(|lib| {
                        lib.symbols
                            .iter()
                            .any(|s| s.defined && s.name == symbol.name)
                    });
                    let binding = match owner {
                        Some(owner) => Binding::App(owner.name.clone()),
                        None => match super::binding(&symbol.name, &needed_tables, symbol.object) {
                            Binding::Unresolved => crate::unimplemented::find(
                                needed_ndk.iter().copied(),
                                &symbol.name,
                                symbol.object,
                                symbol.weak,
                            )
                            .map_or(Binding::Unresolved, |function| {
                                Binding::Unimplemented(function.library)
                            }),
                            binding => binding,
                        },
                    };
                    Import {
                        name: symbol.name.clone(),
                        weak: symbol.weak,
                        binding,
                    }
                })
                .collect();
            let defined_functions = library.symbols.iter().filter(|s| s.defined && s.function);
            LibraryPlan {
                name: library.name.clone(),
                needed: library.needed.clone(),
                imports,
                jni: defined_functions
                    .clone()
                    .filter(|s| {
                        s.name.starts_with("Java_")
                            || s.name == "JNI_OnLoad"
                            || s.name == "JNI_OnUnload"
                    })
                    .map(|s| s.name.clone())
                    .collect(),
                functions: defined_functions.map(|s| (s.value, s.size)).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segment_offset_past_the_address_space_is_an_error() {
        let segment = ProgramHeader {
            kind: PT_LOAD,
            flags: 0,
            offset: u64::MAX,
            vaddr: 0x1000,
            filesz: 0x100,
            memsz: 0x100,
            align: 0x1000,
        };
        assert!(file_offset(&[segment], 0x1010).is_err());
    }

    #[test]
    fn hostile_files_are_errors_never_panics() {
        assert!(read("x", b"").is_err());
        assert!(read("x", b"\x7fELF").is_err());
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4..7].copy_from_slice(&[2, 1, 1]);
        header[16] = 3;
        header[18] = 183;
        header[54] = 56;
        // One program header, said to be far past the end of the file.
        header[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        header[56] = 1;
        assert!(read("x", &header).is_err());
        header[18] = 62;
        assert_eq!(
            read("x", &header),
            Err(ElfError::Unsupported("not an arm64 library"))
        );
    }

    #[test]
    fn a_crafted_gnu_hash_cannot_run_away() {
        let load = ProgramHeader {
            kind: PT_LOAD,
            flags: 0,
            offset: 0,
            vaddr: 0,
            filesz: 64,
            memsz: 64,
            align: 4096,
        };
        // One bucket pointing at symbol 2^20 + 1, with no chain to end it.
        let mut table = vec![0u8; 64];
        table[0..4].copy_from_slice(&1u32.to_le_bytes());
        table[4..8].copy_from_slice(&1u32.to_le_bytes());
        table[16..20].copy_from_slice(&((MAX_SYMBOLS + 1) as u32).to_le_bytes());
        assert!(symbol_count(&table, &[load], 0, 0).is_ok());
        assert!(symbol_count(&table, &[load], 0, 0x0).is_ok());
        let mut file = vec![0u8; 8];
        file.extend(&table);
        let load = ProgramHeader {
            filesz: file.len() as u64,
            ..load
        };
        assert!(symbol_count(&file, &[load], 0, 8).is_err());
    }
}
