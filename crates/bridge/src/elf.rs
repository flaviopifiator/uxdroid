//! Parses ELF64 structures from bytes. Everything here is safe code over untrusted input.

use std::fmt;

pub const EHDR_SIZE: usize = 64;
pub const PHDR_SIZE: usize = 56;
pub const SYM_SIZE: usize = 24;
pub const RELA_SIZE: usize = 24;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const ET_DYN: u16 = 3;
const PAGE_SIZE: u64 = 4096;
/// Android uses at most 64 KiB; the limit keeps address space reservations reasonable.
const MAX_SEGMENT_ALIGN: u64 = 1 << 30;

/// Real libraries need a handful. The limit bounds the memory a crafted file can make us use.
const MAX_NEEDED: usize = 256;

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_RELRO: u32 = 0x6474_E552;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub const STB_WEAK: u8 = 2;
pub const STT_FUNC: u8 = 2;
pub const STT_TLS: u8 = 6;
pub const STT_GNU_IFUNC: u8 = 10;

pub const R_X86_64_64: u32 = 1;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;
pub const R_X86_64_DTPMOD64: u32 = 16;
pub const R_X86_64_DTPOFF64: u32 = 17;
pub const R_X86_64_IRELATIVE: u32 = 37;

pub const R_AARCH64_ABS64: u32 = 257;
pub const R_AARCH64_GLOB_DAT: u32 = 1025;
pub const R_AARCH64_JUMP_SLOT: u32 = 1026;
pub const R_AARCH64_RELATIVE: u32 = 1027;
pub const R_AARCH64_TLS_DTPMOD64: u32 = 1028;
pub const R_AARCH64_TLS_DTPREL64: u32 = 1029;
pub const R_AARCH64_TLS_TPREL64: u32 = 1030;
pub const R_AARCH64_TLSDESC: u32 = 1031;
pub const R_AARCH64_IRELATIVE: u32 = 1032;

#[derive(Debug, PartialEq, Eq)]
pub enum ElfError {
    Malformed(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::Malformed(reason) => write!(f, "malformed ELF: {reason}"),
            ElfError::Unsupported(reason) => write!(f, "unsupported ELF: {reason}"),
        }
    }
}

impl std::error::Error for ElfError {}

type Result<T> = std::result::Result<T, ElfError>;

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(word)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(word)
}

/// The CPU a library is built for. Only the native bridge knows any but the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    X86_64,
    /// `arm64-v8a`, which runs through the translator.
    Aarch64,
}

/// Where the program headers live, read from the ELF header.
#[derive(Debug, PartialEq, Eq)]
pub struct HeaderTable {
    pub machine: Machine,
    pub offset: u64,
    pub count: usize,
}

pub fn parse_header(header: &[u8]) -> Result<HeaderTable> {
    if header.len() < EHDR_SIZE {
        return Err(ElfError::Malformed("truncated header"));
    }
    if &header[..4] != b"\x7fELF" {
        return Err(ElfError::Malformed("bad magic"));
    }
    if header[4] != 2 || header[5] != 1 || header[6] != 1 {
        return Err(ElfError::Unsupported("not little-endian ELF64"));
    }
    if u16_at(header, 16) != ET_DYN {
        return Err(ElfError::Unsupported("not a shared object"));
    }
    let machine = match u16_at(header, 18) {
        EM_X86_64 => Machine::X86_64,
        EM_AARCH64 => Machine::Aarch64,
        _ => return Err(ElfError::Unsupported("not built for x86_64 or arm64")),
    };
    if usize::from(u16_at(header, 54)) != PHDR_SIZE {
        return Err(ElfError::Malformed("bad program header size"));
    }
    Ok(HeaderTable {
        machine,
        offset: u64_at(header, 32),
        count: usize::from(u16_at(header, 56)),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    pub kind: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

impl ProgramHeader {
    /// The end of the segment in memory. `parse_program_headers` proved it does not overflow.
    pub fn end(&self) -> u64 {
        self.vaddr + self.memsz
    }

    pub fn contains(&self, vaddr: u64, len: u64) -> bool {
        vaddr >= self.vaddr
            && vaddr
                .checked_add(len)
                .is_some_and(|end| end <= self.vaddr + self.memsz)
    }
}

pub fn page_floor(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

/// `None` on overflow.
pub fn page_ceil(value: u64) -> Option<u64> {
    Some(value.checked_add(PAGE_SIZE - 1)? & !(PAGE_SIZE - 1))
}

/// Decodes the table and proves the invariants the loader relies on when mapping.
pub fn parse_program_headers(table: &[u8]) -> Result<Vec<ProgramHeader>> {
    let mut headers = Vec::with_capacity(table.len() / PHDR_SIZE);
    let mut previous_load_end = 0;
    let mut has_load = false;

    for raw in table.as_chunks::<PHDR_SIZE>().0 {
        let header = ProgramHeader {
            kind: u32_at(raw, 0),
            flags: u32_at(raw, 4),
            offset: u64_at(raw, 8),
            vaddr: u64_at(raw, 16),
            filesz: u64_at(raw, 32),
            memsz: u64_at(raw, 40),
            align: u64_at(raw, 48),
        };
        if header.filesz > header.memsz
            && matches!(header.kind, PT_LOAD | PT_DYNAMIC | PT_GNU_RELRO)
        {
            return Err(ElfError::Malformed("segment larger on disk than in memory"));
        }
        let end = header.vaddr.checked_add(header.memsz);
        if end.and_then(page_ceil).is_none() || header.offset.checked_add(header.filesz).is_none() {
            return Err(ElfError::Malformed("segment range overflows"));
        }
        if header.kind == PT_LOAD {
            if header.flags & PF_W != 0 && header.flags & PF_X != 0 {
                return Err(ElfError::Unsupported(
                    "segment both writable and executable",
                ));
            }
            if header.vaddr % PAGE_SIZE != header.offset % PAGE_SIZE {
                return Err(ElfError::Malformed(
                    "segment offset and address not congruent",
                ));
            }
            if header.align > MAX_SEGMENT_ALIGN
                || (header.align != 0 && !header.align.is_power_of_two())
            {
                return Err(ElfError::Malformed("bad segment alignment"));
            }
            if has_load && page_floor(header.vaddr) < previous_load_end {
                return Err(ElfError::Malformed("overlapping or unsorted segments"));
            }
            previous_load_end = page_ceil(header.end()).unwrap_or(u64::MAX);
            has_load = true;
        }
        headers.push(header);
    }

    if !has_load {
        return Err(ElfError::Malformed("no loadable segments"));
    }
    Ok(headers)
}

/// Values of the dynamic section. All addresses are unrelocated virtual addresses.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dynamic {
    pub needed: Vec<u64>,
    pub strtab: u64,
    pub strsz: u64,
    pub symtab: u64,
    pub hash: u64,
    pub gnu_hash: u64,
    pub rela: u64,
    pub relasz: u64,
    pub jmprel: u64,
    pub pltrelsz: u64,
    pub android_rela: u64,
    pub android_relasz: u64,
    pub relr: u64,
    pub relrsz: u64,
    pub init: u64,
    pub init_array: u64,
    pub init_arraysz: u64,
}

/// Reads the dynamic section of an x86_64 library.
pub fn parse_dynamic(section: &[u8]) -> Result<Dynamic> {
    parse_dynamic_for(section, Machine::X86_64)
}

/// Reads the dynamic section of a library for `machine`, which only changes the errors' wording.
pub fn parse_dynamic_for(section: &[u8], machine: Machine) -> Result<Dynamic> {
    let rel = match machine {
        Machine::X86_64 => "REL relocations on x86_64",
        Machine::Aarch64 => "REL relocations on arm64",
    };
    const DF_TEXTREL: u64 = 4;
    const DT_RELA_TAG: u64 = 7;

    let mut dynamic = Dynamic::default();
    for entry in section.as_chunks::<16>().0 {
        let tag = u64_at(entry, 0);
        let value = u64_at(entry, 8);
        match tag {
            0 => return Ok(dynamic),
            1 => {
                if dynamic.needed.len() == MAX_NEEDED {
                    return Err(ElfError::Malformed("too many direct dependencies"));
                }
                dynamic.needed.push(value);
            }
            2 => dynamic.pltrelsz = value,
            4 => dynamic.hash = value,
            5 => dynamic.strtab = value,
            6 => dynamic.symtab = value,
            7 => dynamic.rela = value,
            8 => dynamic.relasz = value,
            10 => dynamic.strsz = value,
            12 => dynamic.init = value,
            17 | 18 => {
                if value != 0 {
                    return Err(ElfError::Unsupported(rel));
                }
            }
            20 => {
                if value != DT_RELA_TAG {
                    return Err(ElfError::Unsupported(rel));
                }
            }
            22 => return Err(ElfError::Unsupported("text relocations")),
            23 => dynamic.jmprel = value,
            25 => dynamic.init_array = value,
            27 => dynamic.init_arraysz = value,
            30 => {
                if value & DF_TEXTREL != 0 {
                    return Err(ElfError::Unsupported("text relocations"));
                }
            }
            35 | 0x6FFF_E001 => dynamic.relrsz = value,
            36 | 0x6FFF_E000 => dynamic.relr = value,
            0x6000_0011 => dynamic.android_rela = value,
            0x6000_0012 => dynamic.android_relasz = value,
            0x6FFF_FEF5 => dynamic.gnu_hash = value,
            _ => {}
        }
    }
    Err(ElfError::Malformed("dynamic section has no terminator"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symbol {
    pub name: u32,
    pub bind: u8,
    pub kind: u8,
    pub section: u16,
    pub value: u64,
}

impl Symbol {
    pub fn parse(raw: &[u8]) -> Result<Symbol> {
        if raw.len() < SYM_SIZE {
            return Err(ElfError::Malformed("truncated symbol"));
        }
        Ok(Symbol {
            name: u32_at(raw, 0),
            bind: raw[4] >> 4,
            kind: raw[4] & 0xF,
            section: u16_at(raw, 6),
            value: u64_at(raw, 8),
        })
    }

    pub fn is_defined(&self) -> bool {
        self.section != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rela {
    pub offset: u64,
    pub kind: u32,
    pub symbol: u32,
    pub addend: i64,
}

impl Rela {
    fn from_info(offset: u64, info: u64, addend: i64) -> Rela {
        Rela {
            offset,
            kind: info as u32,
            symbol: (info >> 32) as u32,
            addend,
        }
    }

    pub fn parse(raw: &[u8]) -> Rela {
        Rela::from_info(u64_at(raw, 0), u64_at(raw, 8), u64_at(raw, 16) as i64)
    }
}

/// Decodes the "APS2" packed relocation format that `--pack-dyn-relocs=android` produces.
///
/// `limit` caps the declared count, and nothing is buffered: a few bytes can declare billions of
/// relocations, and `apply` rejects the first one outside the image.
pub fn for_each_packed(
    data: &[u8],
    limit: u64,
    mut apply: impl FnMut(Rela) -> Result<()>,
) -> Result<()> {
    const GROUPED_BY_INFO: i64 = 1;
    const GROUPED_BY_OFFSET_DELTA: i64 = 2;
    const GROUPED_BY_ADDEND: i64 = 4;
    const GROUP_HAS_ADDEND: i64 = 8;

    let Some(body) = data.strip_prefix(b"APS2") else {
        return Err(ElfError::Malformed("bad packed relocation magic"));
    };
    let mut reader = Sleb128 { data: body };
    let count = reader.next_value()?;
    if count < 0 || count as u64 > limit {
        return Err(ElfError::Malformed("bad packed relocation count"));
    }
    let mut remaining = count as u64;
    let mut offset = reader.next_value()? as u64;
    let mut info = 0;
    let mut addend: i64 = 0;

    while remaining > 0 {
        let group_size = reader.next_value()?;
        if group_size <= 0 || group_size as u64 > remaining {
            return Err(ElfError::Malformed("bad packed relocation group"));
        }
        let flags = reader.next_value()?;
        let group_offset_delta = if flags & GROUPED_BY_OFFSET_DELTA != 0 {
            Some(reader.next_value()? as u64)
        } else {
            None
        };
        if flags & GROUPED_BY_INFO != 0 {
            info = reader.next_value()? as u64;
        }
        let has_addend = flags & GROUP_HAS_ADDEND != 0;
        if has_addend && flags & GROUPED_BY_ADDEND != 0 {
            addend = addend.wrapping_add(reader.next_value()?);
        } else if !has_addend {
            addend = 0;
        }

        for _ in 0..group_size {
            let delta = match group_offset_delta {
                Some(delta) => delta,
                None => reader.next_value()? as u64,
            };
            offset = offset.wrapping_add(delta);
            if flags & GROUPED_BY_INFO == 0 {
                info = reader.next_value()? as u64;
            }
            if has_addend && flags & GROUPED_BY_ADDEND == 0 {
                addend = addend.wrapping_add(reader.next_value()?);
            }
            apply(Rela::from_info(offset, info, addend))?;
        }
        remaining -= group_size as u64;
    }
    Ok(())
}

struct Sleb128<'a> {
    data: &'a [u8],
}

impl Sleb128<'_> {
    fn next_value(&mut self) -> Result<i64> {
        let mut value: i64 = 0;
        let mut shift = 0;
        loop {
            let Some((&byte, rest)) = self.data.split_first() else {
                return Err(ElfError::Malformed("truncated packed relocations"));
            };
            self.data = rest;
            if shift >= 64 {
                return Err(ElfError::Malformed("oversized packed relocation value"));
            }
            value |= i64::from(byte & 0x7F) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                return Ok(value);
            }
        }
    }
}

/// Calls `apply` with the address of every relocation in a RELR table.
pub fn for_each_relr(table: &[u8], mut apply: impl FnMut(u64) -> Result<()>) -> Result<()> {
    const WORD: u64 = 8;
    const BITMAP_SLOTS: u64 = 63;

    let mut next = 0u64;
    for raw in table.as_chunks::<8>().0 {
        let entry = u64_at(raw, 0);
        if entry & 1 == 0 {
            apply(entry)?;
            next = entry
                .checked_add(WORD)
                .ok_or(ElfError::Malformed("RELR address overflows"))?;
        } else {
            let mut bits = entry >> 1;
            while bits != 0 {
                let slot = u64::from(bits.trailing_zeros());
                let address = slot
                    .checked_mul(WORD)
                    .and_then(|delta| next.checked_add(delta))
                    .ok_or(ElfError::Malformed("RELR address overflows"))?;
                apply(address)?;
                bits &= bits - 1;
            }
            next = next
                .checked_add(BITMAP_SLOTS * WORD)
                .ok_or(ElfError::Malformed("RELR address overflows"))?;
        }
    }
    Ok(())
}

pub fn gnu_hash(name: &[u8]) -> u32 {
    name.iter().fold(5381u32, |hash, &byte| {
        hash.wrapping_mul(33).wrapping_add(u32::from(byte))
    })
}

pub fn sysv_hash(name: &[u8]) -> u32 {
    let mut hash = 0u32;
    for &byte in name {
        hash = (hash << 4).wrapping_add(u32::from(byte));
        let high = hash & 0xF000_0000;
        hash ^= high >> 24;
        hash &= !high;
    }
    hash
}
