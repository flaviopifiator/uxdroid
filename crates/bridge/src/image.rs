//! Maps a shared object into memory. All access to mapped bytes goes through bounds checks.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr;

use crate::elf::{
    ElfError, PF_R, PF_W, PF_X, PT_GNU_RELRO, PT_LOAD, ProgramHeader, page_ceil, page_floor,
};

/// Longer than any real symbol or library name.
const MAX_STRING_BYTES: u64 = 64 * 1024;

/// A mapped library. The mapping lives until `unmap`; loaded libraries are never unloaded.
#[derive(Debug)]
pub struct Image {
    bias: usize,
    reservation: usize,
    reservation_len: usize,
    loads: Vec<ProgramHeader>,
}

fn os_error(what: &'static str) -> ElfError {
    ElfError::Unsupported(what)
}

impl Image {
    /// Reserves address space for the whole library, then maps each segment into it.
    pub fn map(file: &File, file_len: u64, headers: &[ProgramHeader]) -> Result<Image, ElfError> {
        let loads: Vec<ProgramHeader> = headers
            .iter()
            .filter(|header| header.kind == PT_LOAD)
            .copied()
            .collect();
        let (Some(first), Some(last)) = (loads.first(), loads.last()) else {
            return Err(ElfError::Malformed("no loadable segments"));
        };
        for load in &loads {
            if load.offset.saturating_add(load.filesz) > file_len {
                return Err(ElfError::Malformed("segment extends past end of file"));
            }
        }
        let start = page_floor(first.vaddr);
        let end = page_ceil(last.end()).ok_or(ElfError::Malformed("segment range overflows"))?;
        let align = loads
            .iter()
            .map(|load| load.align)
            .max()
            .unwrap_or(0)
            .max(4096);
        let span =
            usize::try_from(end - start).map_err(|_| ElfError::Malformed("library too large"))?;
        let reservation_len = span
            .checked_add(align as usize)
            .ok_or(ElfError::Malformed("library too large"))?;

        // SAFETY: a new anonymous mapping at a kernel-chosen address touches no existing memory.
        let reservation = unsafe {
            libc::mmap(
                ptr::null_mut(),
                reservation_len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if reservation == libc::MAP_FAILED {
            return Err(os_error("cannot reserve address space"));
        }
        let aligned = (reservation as usize + align as usize - 1) & !(align as usize - 1);
        let image = Image {
            bias: aligned.wrapping_sub(start as usize),
            reservation: reservation as usize,
            reservation_len,
            loads,
        };
        match image.map_segments(file) {
            Ok(()) => Ok(image),
            Err(error) => {
                image.unmap();
                Err(error)
            }
        }
    }

    fn map_segments(&self, file: &File) -> Result<(), ElfError> {
        for load in &self.loads {
            let protection = protection_of(load.flags);
            let page_start = page_floor(load.vaddr);
            let file_end = load.vaddr + load.filesz;
            let mapped_file_end = if load.filesz == 0 {
                page_start
            } else {
                page_ceil(file_end).ok_or(ElfError::Malformed("segment range overflows"))?
            };

            if load.filesz > 0 {
                // SAFETY: the target lies inside this image's reservation, which `map` sized to
                // hold every segment, and MAP_FIXED replaces only that reservation.
                let mapped = unsafe {
                    libc::mmap(
                        self.bias.wrapping_add(page_start as usize) as *mut _,
                        (mapped_file_end - page_start) as usize,
                        protection,
                        libc::MAP_PRIVATE | libc::MAP_FIXED,
                        file.as_raw_fd(),
                        page_floor(load.offset) as libc::off_t,
                    )
                };
                if mapped == libc::MAP_FAILED {
                    let denied = matches!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EPERM | libc::EACCES)
                    );
                    // A noexec mount refuses executable file mappings; such a segment is read
                    // into anonymous memory instead, which costs its sharing between processes.
                    if !(denied && protection & libc::PROT_EXEC != 0) {
                        return Err(os_error("cannot map segment"));
                    }
                    self.copy_segment(
                        file,
                        page_start,
                        mapped_file_end - page_start,
                        page_floor(load.offset),
                        protection,
                    )?;
                }
            }

            if load.memsz > load.filesz && load.flags & PF_W != 0 && file_end < mapped_file_end {
                // SAFETY: this range is the tail of the last page mapped above, which is writable.
                unsafe {
                    ptr::write_bytes(
                        self.bias.wrapping_add(file_end as usize) as *mut u8,
                        0,
                        (mapped_file_end - file_end) as usize,
                    );
                }
            }

            let segment_end =
                page_ceil(load.end()).ok_or(ElfError::Malformed("segment range overflows"))?;
            if segment_end > mapped_file_end {
                // SAFETY: as above, the target lies inside the reservation; anonymous pages
                // provide the zeroed memory of `.bss`.
                let mapped = unsafe {
                    libc::mmap(
                        self.bias.wrapping_add(mapped_file_end as usize) as *mut _,
                        (segment_end - mapped_file_end) as usize,
                        protection,
                        libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                if mapped == libc::MAP_FAILED {
                    return Err(os_error("cannot map zeroed segment"));
                }
            }
        }
        Ok(())
    }

    /// Maps `len` bytes at `vaddr` as anonymous memory holding the file's bytes from `offset`, the
    /// part past the end of the file zeroed, then gives it its final protection.
    fn copy_segment(
        &self,
        file: &File,
        vaddr: u64,
        len: u64,
        offset: u64,
        protection: i32,
    ) -> Result<(), ElfError> {
        use std::os::unix::fs::FileExt;

        let address = self.bias.wrapping_add(vaddr as usize);
        // SAFETY: the target lies inside this image's reservation, as for the file mapping it
        // replaces, and MAP_FIXED replaces only that reservation.
        let mapped = unsafe {
            libc::mmap(
                address as *mut _,
                len as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(os_error("cannot map segment"));
        }
        // SAFETY: the anonymous mapping just made is `len` writable bytes that nothing else uses.
        let bytes = unsafe { std::slice::from_raw_parts_mut(address as *mut u8, len as usize) };
        let mut filled = 0;
        while filled < bytes.len() {
            match file.read_at(&mut bytes[filled..], offset + filled as u64) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(os_error("cannot read segment")),
            }
        }
        // SAFETY: the same range, mapped above; only its protection changes.
        if unsafe { libc::mprotect(address as *mut _, len as usize, protection) } != 0 {
            return Err(os_error("cannot protect segment"));
        }
        Ok(())
    }

    pub fn unmap(&self) {
        // SAFETY: this range is the reservation made by `map`, and nothing has run from it yet.
        unsafe {
            libc::munmap(self.reservation as *mut _, self.reservation_len);
        }
    }

    pub fn bias(&self) -> usize {
        self.bias
    }

    /// The readable bytes at `vaddr`, or an error when any of them is outside a readable segment.
    pub fn bytes(&self, vaddr: u64, len: u64) -> Result<&[u8], ElfError> {
        let readable = self
            .loads
            .iter()
            .any(|load| load.flags & PF_R != 0 && load.contains(vaddr, len));
        if !readable {
            return Err(ElfError::Malformed("address outside readable segments"));
        }
        // SAFETY: the check above proves the range lies inside one mapped, readable segment, and
        // `self` keeps the mapping alive.
        Ok(unsafe {
            std::slice::from_raw_parts(
                self.bias.wrapping_add(vaddr as usize) as *const u8,
                len as usize,
            )
        })
    }

    /// The NUL-terminated string at `offset` of a string table, without the terminator.
    pub fn string(&self, table: u64, table_len: u64, offset: u64) -> Result<&[u8], ElfError> {
        if offset >= table_len {
            return Err(ElfError::Malformed("string offset past table"));
        }
        // Bounded, so a crafted table cannot cost its whole size on every lookup.
        let bytes = self.bytes(
            table
                .checked_add(offset)
                .ok_or(ElfError::Malformed("string offset past table"))?,
            (table_len - offset).min(MAX_STRING_BYTES),
        )?;
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(ElfError::Malformed("unterminated or oversized string"))?;
        Ok(&bytes[..end])
    }

    pub fn write_u64(&self, vaddr: u64, value: u64) -> Result<(), ElfError> {
        let writable = self
            .loads
            .iter()
            .any(|load| load.flags & PF_W != 0 && load.contains(vaddr, 8));
        if !writable {
            return Err(ElfError::Malformed("relocation outside writable segments"));
        }
        // SAFETY: the check above proves the 8 bytes lie inside one mapped, writable segment.
        unsafe {
            ptr::write_unaligned(self.bias.wrapping_add(vaddr as usize) as *mut u64, value);
        }
        Ok(())
    }

    pub fn read_u64(&self, vaddr: u64) -> Result<u64, ElfError> {
        let mut word = [0; 8];
        word.copy_from_slice(self.bytes(vaddr, 8)?);
        Ok(u64::from_le_bytes(word))
    }

    pub fn is_executable(&self, vaddr: u64) -> bool {
        self.loads
            .iter()
            .any(|load| load.flags & PF_X != 0 && load.contains(vaddr, 1))
    }

    /// Whether the absolute address falls inside one of this library's segments.
    pub fn contains_address(&self, address: usize) -> bool {
        let vaddr = address.wrapping_sub(self.bias) as u64;
        self.loads.iter().any(|load| load.contains(vaddr, 1))
    }

    /// Makes the relocated read-only data truly read-only.
    pub fn protect_relro(&self, headers: &[ProgramHeader]) -> Result<(), ElfError> {
        for header in headers.iter().filter(|header| header.kind == PT_GNU_RELRO) {
            let start = page_floor(header.vaddr);
            let end = page_floor(header.end());
            if end <= start {
                continue;
            }
            let inside = self.loads.iter().any(|load| {
                load.flags & PF_W != 0
                    && page_floor(load.vaddr) <= start
                    && end <= page_ceil(load.end()).unwrap_or(0)
            });
            if !inside {
                return Err(ElfError::Malformed("RELRO outside writable segments"));
            }
            // SAFETY: the range is page-aligned and inside this image, and no code has run yet
            // that could depend on writing to it.
            let status = unsafe {
                libc::mprotect(
                    self.bias.wrapping_add(start as usize) as *mut _,
                    (end - start) as usize,
                    libc::PROT_READ,
                )
            };
            if status != 0 {
                return Err(os_error("cannot protect RELRO"));
            }
        }
        Ok(())
    }
}

fn protection_of(flags: u32) -> libc::c_int {
    let mut protection = libc::PROT_NONE;
    if flags & PF_R != 0 {
        protection |= libc::PROT_READ;
    }
    if flags & PF_W != 0 {
        protection |= libc::PROT_WRITE;
    }
    if flags & PF_X != 0 {
        protection |= libc::PROT_EXEC;
    }
    protection
}
