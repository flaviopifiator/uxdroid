//! `android.os.Parcel` in Rust: Android's wire format, with binders and file descriptors as
//! in-process objects.
//!
//! The layout follows AOSP's `frameworks/native/libs/binder/Parcel.cpp` (Android 16, Apache-2.0)
//! for a kernel-binder parcel: little-endian values at 4-byte alignment, strings as a length, the
//! characters, a terminator and zero padding, and objects as 24-byte `flat_binder_object` records
//! whose offsets the parcel remembers. Nothing leaves the process, so an object is the Java object
//! or file itself, kept in a table beside the bytes: a binder is found only where it was written,
//! and bytes from outside, such as an unmarshalled array, never hold one.
//!
//! Every length and offset read from a parcel is checked against its data before use, so hostile
//! bytes read as an error, never as memory past the end, and nothing allocates more than the data
//! already holds. Buffers grow as Android's do, by half of what they need, from 128 bytes.


use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};


/// A `status_t` from `utils/Errors.h`.
pub type Status = i32;
pub const PERMISSION_DENIED: Status = -1;
pub const NO_MEMORY: Status = -12;
pub const BAD_VALUE: Status = -22;
pub const INVALID_OPERATION: Status = -38;
pub const NOT_ENOUGH_DATA: Status = -61;
pub const BAD_TYPE: Status = i32::MIN + 1;
pub const FDS_NOT_ALLOWED: Status = i32::MIN + 7;

/// `sizeof(flat_binder_object)`.
pub const OBJECT_SIZE: usize = 24;
/// `B_PACK_CHARS('s', 'b', '*', B_TYPE_LARGE)` and `B_PACK_CHARS('f', 'd', '*', B_TYPE_LARGE)`.
const TYPE_BINDER: u32 = 0x7362_2a85;
const TYPE_FD: u32 = 0x6664_2a85;
/// `FLAT_BINDER_FLAG_ACCEPTS_FDS`, and the scheduling bits of `SCHED_NORMAL` at priority 19.
const FLAG_ACCEPTS_FDS: u32 = 0x100;
const SCHED_BITS: u32 = 0x13;
/// The stability word after each binder: `Stability::Level::SYSTEM`, as the framework's own.
const STABILITY_SYSTEM: i32 = 12;
/// The request header of `writeInterfaceToken`: `STRICT_MODE_PENALTY_GATHER`, an unset work
/// source and `kHeader`, `B_PACK_CHARS('S', 'Y', 'S', 'T')` for system code.
const STRICT_MODE_PENALTY_GATHER: i32 = i32::MIN;
const UNSET_WORK_SOURCE: i32 = -1;
const HEADER: i32 = 0x5359_5354;
/// `BLOB_INPLACE`: blobs always travel inside the parcel, since no other process maps them.
const BLOB_INPLACE: i32 = 0;
/// The largest size or position Android accepts, so a negative `int` never becomes a huge size.
const LIMIT: usize = i32::MAX as usize;

static ALLOC_SIZE: AtomicU64 = AtomicU64::new(0);
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Bytes held by every parcel buffer of the process, for `Parcel.getGlobalAllocSize`.
pub fn global_alloc_size() -> u64 {
    ALLOC_SIZE.load(AtomicOrdering::Relaxed)
}

/// Parcels holding a buffer, for `Parcel.getGlobalAllocCount`.
pub fn global_alloc_count() -> u64 {
    ALLOC_COUNT.load(AtomicOrdering::Relaxed)
}

fn pad(length: usize) -> Option<usize> {
    Some(length.checked_add(3)? & !3)
}

#[derive(Clone)]
enum Object<B> {
    Binder(B),
    Fd(Arc<OwnedFd>),
}

/// A parcel whose binders are values of `B`, such as references to Java objects.
pub struct Parcel<B> {
    /// The buffer; its length is the capacity, and bytes past `size` are zero or stale.
    buffer: Vec<u8>,
    size: usize,
    position: usize,
    objects: BTreeMap<usize, Object<B>>,
    allow_fds: bool,
    /// Where `writeInterfaceToken` or `enforceInterface` found the work source.
    work_source_at: Option<usize>,
}

impl<B: Clone> Default for Parcel<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: Clone> Clone for Parcel<B> {
    fn clone(&self) -> Self {
        let mut copy = Parcel {
            buffer: Vec::new(),
            size: self.size,
            position: self.position,
            objects: self.objects.clone(),
            allow_fds: self.allow_fds,
            work_source_at: self.work_source_at,
        };
        copy.account(self.buffer.len());
        copy.buffer = self.buffer.clone();
        copy
    }
}

impl<B> Parcel<B> {
    /// Records a capacity change in the process totals.
    fn account(&self, new: usize) {
        let old = self.buffer.len() as u64;
        let new = new as u64;
        if new >= old {
            ALLOC_SIZE.fetch_add(new - old, AtomicOrdering::Relaxed);
        } else {
            ALLOC_SIZE.fetch_sub(old - new, AtomicOrdering::Relaxed);
        }
        if old == 0 && new > 0 {
            ALLOC_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
        } else if old > 0 && new == 0 {
            ALLOC_COUNT.fetch_sub(1, AtomicOrdering::Relaxed);
        }
    }
}

impl<B> Drop for Parcel<B> {
    fn drop(&mut self) {
        self.account(0);
    }
}

impl<B: Clone> Parcel<B> {
    pub fn new() -> Self {
        Parcel {
            buffer: Vec::new(),
            size: 0,
            position: 0,
            objects: BTreeMap::new(),
            allow_fds: true,
            work_source_at: None,
        }
    }

    /// The buffer up to its capacity.
    pub fn data(&self) -> &[u8] {
        &self.buffer
    }

    /// `dataSize()`: a position past the end counts as the size.
    pub fn data_size(&self) -> usize {
        self.size.max(self.position)
    }

    pub fn data_position(&self) -> usize {
        self.position
    }

    pub fn data_capacity(&self) -> usize {
        self.buffer.len()
    }

    pub fn data_avail(&self) -> usize {
        self.data_size() - self.position
    }

    pub fn set_data_position(&mut self, position: usize) -> Result<(), Status> {
        if position > LIMIT {
            return Err(BAD_VALUE);
        }
        self.position = position;
        Ok(())
    }

    pub fn set_data_size(&mut self, size: usize) -> Result<(), Status> {
        self.continue_write(size)?;
        self.size = size;
        self.position = self.position.min(size);
        Ok(())
    }

    /// Grows the buffer to `capacity`; never shrinks it.
    pub fn set_data_capacity(&mut self, capacity: usize) -> Result<(), Status> {
        if capacity > LIMIT {
            return Err(BAD_VALUE);
        }
        if capacity > self.buffer.len() {
            self.continue_write(capacity)?;
        }
        Ok(())
    }

    /// `continueWrite`: makes room for `desired` bytes, or cuts the data and its objects to it.
    fn continue_write(&mut self, desired: usize) -> Result<(), Status> {
        if desired > LIMIT {
            return Err(BAD_VALUE);
        }
        self.objects
            .retain(|&offset, _| offset + OBJECT_SIZE <= desired);
        if desired > self.buffer.len() {
            let more = desired - self.buffer.len();
            self.buffer.try_reserve_exact(more).map_err(|_| NO_MEMORY)?;
            self.account(desired);
            self.buffer.resize(desired, 0);
        } else {
            self.size = self.size.min(desired);
            self.position = self.position.min(desired);
        }
        Ok(())
    }

    /// `growData`: only at the end of the data, by half again what is needed.
    fn grow(&mut self, length: usize) -> Result<(), Status> {
        if length > LIMIT || self.position > self.size {
            return Err(BAD_VALUE);
        }
        let needed = self.size.checked_add(length).ok_or(NO_MEMORY)?;
        let capacity = (needed.checked_mul(3).ok_or(NO_MEMORY)? / 2).max(128);
        self.continue_write(capacity)
    }

    /// Room for `length` bytes at the position, which then moves past them.
    fn reserve_write(&mut self, length: usize) -> Result<usize, Status> {
        let start = self.position;
        if start
            .checked_add(length)
            .is_none_or(|end| end > self.buffer.len())
        {
            self.grow(length)?;
        }
        self.position += length;
        self.size = self.size.max(self.position);
        Ok(start)
    }

    fn write_aligned<const N: usize>(&mut self, bytes: [u8; N]) -> Result<(), Status> {
        let start = self.reserve_write(N)?;
        self.buffer[start..start + N].copy_from_slice(&bytes);
        Ok(())
    }

    pub fn write_i32(&mut self, value: i32) -> Result<(), Status> {
        self.write_aligned(value.to_le_bytes())
    }

    pub fn write_i64(&mut self, value: i64) -> Result<(), Status> {
        self.write_aligned(value.to_le_bytes())
    }

    pub fn write_f32(&mut self, value: f32) -> Result<(), Status> {
        self.write_aligned(value.to_le_bytes())
    }

    pub fn write_f64(&mut self, value: f64) -> Result<(), Status> {
        self.write_aligned(value.to_le_bytes())
    }

    /// `writeInplace`: `length` bytes padded to 4, the padding zeroed.
    fn write_inplace(&mut self, length: usize) -> Result<&mut [u8], Status> {
        let padded = pad(length).filter(|_| length <= LIMIT).ok_or(BAD_VALUE)?;
        let start = self.reserve_write(padded)?;
        self.buffer[start + length..start + padded].fill(0);
        Ok(&mut self.buffer[start..start + length])
    }

    /// Plain data must not overlap an object, as `validateReadData` checks.
    fn overlaps_object(&self, start: usize, end: usize) -> bool {
        self.objects
            .range(start.saturating_sub(OBJECT_SIZE - 1)..end)
            .next()
            .is_some()
    }

    /// `readInplace`: `length` bytes and their padding, all inside the data.
    fn read_inplace(&mut self, length: usize) -> Option<&[u8]> {
        let padded = pad(length).filter(|_| length <= LIMIT)?;
        let start = self.position;
        let end = start.checked_add(padded).filter(|&end| end <= self.size)?;
        self.position = end;
        if self.overlaps_object(start, end) {
            return None;
        }
        Some(&self.buffer[start..start + length])
    }

    fn read_aligned<const N: usize>(&mut self) -> Result<[u8; N], Status> {
        let start = self.position;
        let end = start
            .checked_add(N)
            .filter(|&end| end <= self.size)
            .ok_or(NOT_ENOUGH_DATA)?;
        self.position = end;
        if self.overlaps_object(start, end) {
            return Err(PERMISSION_DENIED);
        }
        let mut bytes = [0; N];
        bytes.copy_from_slice(&self.buffer[start..end]);
        Ok(bytes)
    }

    pub fn read_i32(&mut self) -> Result<i32, Status> {
        self.read_aligned().map(i32::from_le_bytes)
    }

    pub fn read_i64(&mut self) -> Result<i64, Status> {
        self.read_aligned().map(i64::from_le_bytes)
    }

    pub fn read_f32(&mut self) -> Result<f32, Status> {
        self.read_aligned().map(f32::from_le_bytes)
    }

    pub fn read_f64(&mut self) -> Result<f64, Status> {
        self.read_aligned().map(f64::from_le_bytes)
    }

    /// A length as `readInt32()` gives it: zero when there is none.
    fn read_length(&mut self) -> i32 {
        self.read_i32().unwrap_or(0)
    }

    pub fn write_string16(&mut self, text: Option<&[u16]>) -> Result<(), Status> {
        let Some(text) = text else {
            return self.write_i32(-1);
        };
        let length = i32::try_from(text.len()).map_err(|_| BAD_VALUE)?;
        self.write_i32(length)?;
        let bytes = self.write_inplace((text.len() + 1) * 2)?;
        let (pairs, _) = bytes.as_chunks_mut::<2>();
        for (unit, out) in text.iter().chain(&[0]).zip(pairs) {
            *out = unit.to_le_bytes();
        }
        Ok(())
    }

    /// `readString16Inplace`: `None` for null, a bad length or a missing terminator.
    pub fn read_string16(&mut self) -> Option<Vec<u16>> {
        let length = usize::try_from(self.read_length())
            .ok()
            .filter(|&n| n < LIMIT)?;
        let bytes = self.read_inplace((length + 1) * 2)?;
        let (pairs, _) = bytes.as_chunks::<2>();
        let (text, terminator) = pairs.split_at(length);
        (terminator.first() == Some(&[0, 0]))
            .then(|| text.iter().map(|&pair| u16::from_le_bytes(pair)).collect())
    }

    pub fn write_string8(&mut self, text: Option<&[u8]>) -> Result<(), Status> {
        let Some(text) = text else {
            return self.write_i32(-1);
        };
        let length = i32::try_from(text.len()).map_err(|_| BAD_VALUE)?;
        self.write_i32(length)?;
        let bytes = self.write_inplace(text.len() + 1)?;
        bytes[..text.len()].copy_from_slice(text);
        bytes[text.len()] = 0;
        Ok(())
    }

    /// `readString8Inplace`: the bytes without their terminator.
    pub fn read_string8(&mut self) -> Option<&[u8]> {
        let length = usize::try_from(self.read_length())
            .ok()
            .filter(|&n| n < LIMIT)?;
        let bytes = self.read_inplace(length + 1)?;
        (bytes[length] == 0).then(|| &bytes[..length])
    }

    pub fn write_byte_array(&mut self, bytes: Option<&[u8]>) -> Result<(), Status> {
        let Some(bytes) = bytes else {
            return self.write_i32(-1);
        };
        self.write_i32(i32::try_from(bytes.len()).map_err(|_| BAD_VALUE)?)?;
        self.write_inplace(bytes.len())?.copy_from_slice(bytes);
        Ok(())
    }

    pub fn write_blob(&mut self, bytes: Option<&[u8]>) -> Result<(), Status> {
        let Some(bytes) = bytes else {
            return self.write_i32(-1);
        };
        self.write_i32(i32::try_from(bytes.len()).map_err(|_| BAD_VALUE)?)?;
        self.write_i32(BLOB_INPLACE)?;
        self.write_inplace(bytes.len())?.copy_from_slice(bytes);
        Ok(())
    }

    /// `createByteArray`: a length no larger than the data left, then the bytes.
    pub fn create_byte_array(&mut self) -> Option<&[u8]> {
        let length = usize::try_from(self.read_length()).ok()?;
        if length > self.data_avail() {
            return None;
        }
        self.read_inplace(length)
    }

    /// `readByteArray` into an array of `expected` bytes, which must be the length found.
    pub fn read_byte_array(&mut self, expected: usize) -> Option<&[u8]> {
        let length = usize::try_from(self.read_length()).ok()?;
        if length > self.data_avail() || length != expected {
            return None;
        }
        self.read_inplace(length)
    }

    /// `readBlob`: `Ok(None)` for null. Only blobs in place exist in one process.
    pub fn read_blob(&mut self) -> Result<Option<&[u8]>, Status> {
        let Ok(length) = usize::try_from(self.read_length()) else {
            return Ok(None);
        };
        if self.read_i32()? != BLOB_INPLACE {
            return Err(BAD_VALUE);
        }
        self.read_inplace(length).map(Some).ok_or(BAD_VALUE)
    }

    pub fn write_interface_token(&mut self, descriptor: &[u16]) -> Result<(), Status> {
        self.write_i32(STRICT_MODE_PENALTY_GATHER)?;
        self.work_source_at = Some(self.position);
        self.write_i32(UNSET_WORK_SOURCE)?;
        self.write_i32(HEADER)?;
        self.write_string16(Some(descriptor))
    }

    /// Reads the request header and checks the descriptor, as a service does on each call.
    pub fn enforce_interface(&mut self, descriptor: &[u16]) -> bool {
        let _strict_mode_policy = self.read_length();
        self.work_source_at = Some(self.position);
        let _work_source = self.read_length();
        if self.read_length() != HEADER {
            return false;
        }
        self.read_string16()
            .is_some_and(|found| found == descriptor)
    }

    pub fn replace_calling_work_source_uid(&mut self, uid: i32) -> bool {
        let Some(at) = self.work_source_at else {
            return false;
        };
        let position = self.position;
        self.position = at;
        let written = self.write_i32(uid);
        self.position = position;
        written.is_ok()
    }

    pub fn read_calling_work_source_uid(&mut self) -> i32 {
        let Some(at) = self.work_source_at else {
            return UNSET_WORK_SOURCE;
        };
        let position = self.position;
        self.position = at;
        let uid = self.read_length();
        self.position = position;
        uid
    }

    /// Writes a `flat_binder_object`, remembering its object if there is one.
    fn write_object(
        &mut self,
        kind: u32,
        flags: u32,
        handle: u64,
        cookie: u64,
        object: Option<Object<B>>,
    ) -> Result<(), Status> {
        if matches!(object, Some(Object::Fd(_))) && !self.allow_fds {
            return Err(FDS_NOT_ALLOWED);
        }
        let start = self.reserve_write(OBJECT_SIZE)?;
        let record = &mut self.buffer[start..start + OBJECT_SIZE];
        record[..4].copy_from_slice(&kind.to_le_bytes());
        record[4..8].copy_from_slice(&flags.to_le_bytes());
        record[8..16].copy_from_slice(&handle.to_le_bytes());
        record[16..].copy_from_slice(&cookie.to_le_bytes());
        if let Some(object) = object {
            self.objects.insert(start, object);
        }
        Ok(())
    }

    /// A binder, or null, followed by its stability word. In one process the record only marks
    /// the place; the binder itself stays in the object table.
    pub fn write_binder(&mut self, binder: Option<B>) -> Result<(), Status> {
        match binder {
            Some(binder) => {
                let flags = FLAG_ACCEPTS_FDS | SCHED_BITS;
                self.write_object(TYPE_BINDER, flags, 1, 1, Some(Object::Binder(binder)))?;
                self.write_i32(STABILITY_SYSTEM)
            }
            None => {
                self.write_object(TYPE_BINDER, SCHED_BITS, 0, 0, None)?;
                self.write_i32(0)
            }
        }
    }

    fn record(&self, start: usize) -> (u32, u64, u64) {
        let word = |at: usize| u64::from_le_bytes(self.buffer[at..at + 8].try_into().unwrap());
        let kind = u32::from_le_bytes(self.buffer[start..start + 4].try_into().unwrap());
        (kind, word(start + 8), word(start + 16))
    }

    /// `readObject`: the object recorded exactly at the position, which moves past its record.
    fn read_object(&mut self) -> Option<(usize, (u32, u64, u64))> {
        let start = self.position;
        start
            .checked_add(OBJECT_SIZE)
            .filter(|&end| end <= self.size)?;
        self.position = start + OBJECT_SIZE;
        Some((start, self.record(start)))
    }

    /// `readStrongBinder`: null when the position holds no binder written by this process.
    pub fn read_binder(&mut self) -> Option<B> {
        let (start, (kind, handle, cookie)) = self.read_object()?;
        if handle == 0 && cookie == 0 {
            if kind == TYPE_BINDER {
                let _stability = self.read_i32();
            }
            return None;
        }
        let Some(Object::Binder(binder)) = self.objects.get(&start) else {
            return None;
        };
        let binder = binder.clone();
        let _stability = self.read_i32();
        Some(binder)
    }

    /// Takes ownership of `fd`, as `writeDupFileDescriptor` does of its copy.
    pub fn write_file_descriptor(&mut self, fd: OwnedFd) -> Result<(), Status> {
        use std::os::fd::AsRawFd;
        let raw = fd.as_raw_fd() as u32 as u64;
        self.write_object(TYPE_FD, 0, raw, 1, Some(Object::Fd(Arc::new(fd))))
    }

    /// The file written at the position; the caller duplicates it to hand it out.
    pub fn read_file_descriptor(&mut self) -> Option<Arc<OwnedFd>> {
        let (start, _) = self.read_object()?;
        match self.objects.get(&start) {
            Some(Object::Fd(fd)) => Some(fd.clone()),
            _ => None,
        }
    }

    pub fn has_binders(&self) -> bool {
        self.objects
            .values()
            .any(|o| matches!(o, Object::Binder(_)))
    }

    pub fn has_file_descriptors(&self) -> bool {
        self.objects.values().any(|o| matches!(o, Object::Fd(_)))
    }

    /// Objects wholly inside `offset..offset + length`, which must lie in the data.
    fn objects_in(
        &self,
        offset: usize,
        length: usize,
    ) -> Result<impl Iterator<Item = (&usize, &Object<B>)>, Status> {
        let end = offset
            .checked_add(length)
            .filter(|&end| offset <= LIMIT && length <= LIMIT && end <= self.size)
            .ok_or(BAD_VALUE)?;
        Ok(self
            .objects
            .range(offset..)
            .take_while(move |(at, _)| *at + OBJECT_SIZE <= end))
    }

    pub fn has_file_descriptors_in_range(
        &self,
        offset: usize,
        length: usize,
    ) -> Result<bool, Status> {
        Ok(self
            .objects_in(offset, length)?
            .any(|(_, o)| matches!(o, Object::Fd(_))))
    }

    pub fn has_binders_in_range(&self, offset: usize, length: usize) -> Result<bool, Status> {
        Ok(self
            .objects_in(offset, length)?
            .any(|(_, o)| matches!(o, Object::Binder(_))))
    }

    /// Forbids file descriptors if `allow` is false; returns the previous setting.
    pub fn push_allow_fds(&mut self, allow: bool) -> bool {
        let previous = self.allow_fds;
        if !allow {
            self.allow_fds = false;
        }
        previous
    }

    pub fn restore_allow_fds(&mut self, previous: bool) {
        self.allow_fds = previous;
    }

    /// The data as bytes; a position past the buffer reads as zeros.
    fn marshall_unchecked(&self) -> Vec<u8> {
        let size = self.data_size();
        let mut bytes = self.buffer[..size.min(self.buffer.len())].to_vec();
        bytes.resize(size, 0);
        bytes
    }

    /// The data as bytes, which only a parcel without objects has.
    pub fn marshall(&self) -> Result<Vec<u8>, Status> {
        if !self.objects.is_empty() {
            return Err(INVALID_OPERATION);
        }
        Ok(self.marshall_unchecked())
    }

    /// Replaces the data with `bytes`, which hold no objects, and leaves the position after them.
    pub fn unmarshall(&mut self, bytes: &[u8]) -> Result<(), Status> {
        self.objects.clear();
        self.work_source_at = None;
        self.set_data_size(bytes.len())?;
        self.position = 0;
        self.write_inplace(bytes.len())?.copy_from_slice(bytes);
        Ok(())
    }

    /// Bytes of the data from `start`, then zeros up to `length`.
    fn bytes_from(&self, start: usize, length: usize) -> impl Iterator<Item = u8> + '_ {
        let stored = self.buffer.get(start..).unwrap_or(&[]);
        stored
            .iter()
            .copied()
            .chain(std::iter::repeat(0))
            .take(length)
    }

    /// `compareData`: the shorter parcel first, then the bytes.
    pub fn compare_data(&self, other: &Self) -> i32 {
        let size = self.data_size();
        let order = size
            .cmp(&other.data_size())
            .then_with(|| self.bytes_from(0, size).cmp(other.bytes_from(0, size)));
        order as i32
    }

    pub fn compare_data_in_range(
        &self,
        offset: usize,
        other: &Self,
        other_offset: usize,
        length: usize,
    ) -> Result<bool, Status> {
        let fits = |parcel: &Self, offset: usize| {
            offset <= LIMIT
                && length <= LIMIT
                && offset
                    .checked_add(length)
                    .is_some_and(|end| end <= parcel.size)
        };
        if !fits(self, offset) || !fits(other, other_offset) {
            return Err(BAD_VALUE);
        }
        Ok(self.buffer[offset..offset + length]
            == other.buffer[other_offset..other_offset + length])
    }

    /// `appendFrom`: copies `length` bytes of `other` from `offset`, with the objects wholly inside.
    pub fn append_from(
        &mut self,
        other: &Self,
        offset: usize,
        length: usize,
    ) -> Result<(), Status> {
        if length == 0 {
            return Ok(());
        }
        let objects: Vec<(usize, Object<B>)> = other
            .objects_in(offset, length)?
            .map(|(at, object)| (at - offset, object.clone()))
            .collect();
        if self.position > self.size {
            return Err(BAD_VALUE);
        }
        if self.size + length > self.buffer.len() {
            self.grow(length)?;
        }
        let start = self.position;
        self.buffer[start..start + length].copy_from_slice(&other.buffer[offset..offset + length]);
        self.position += length;
        self.size += length;
        let mut result = Ok(());
        for (at, object) in objects {
            if matches!(object, Object::Fd(_)) && !self.allow_fds {
                result = Err(FDS_NOT_ALLOWED);
                continue;
            }
            self.objects.insert(start + at, object);
        }
        result
    }

    /// `freeData`: back to an empty parcel without a buffer.
    pub fn free_data(&mut self) {
        self.account(0);
        self.buffer = Vec::new();
        self.size = 0;
        self.position = 0;
        self.objects.clear();
        self.allow_fds = true;
        self.work_source_at = None;
    }
}

/// Decodes Java's modified UTF-8, and the 4-byte form other writers use, into UTF-16. Bytes that
/// form no character become U+FFFD; nothing about the input is trusted.
pub fn utf16_from_modified_utf8(bytes: &[u8]) -> Vec<u16> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let continuation = |at: usize| {
        bytes
            .get(at)
            .filter(|&&b| b & 0xc0 == 0x80)
            .map(|&b| u32::from(b & 0x3f))
    };
    while i < bytes.len() {
        let lead = bytes[i];
        let (length, initial) = match lead {
            0x00..=0x7f => (1, u32::from(lead)),
            0xc0..=0xdf => (2, u32::from(lead & 0x1f)),
            0xe0..=0xef => (3, u32::from(lead & 0x0f)),
            0xf0..=0xf4 => (4, u32::from(lead & 0x07)),
            _ => (0, 0),
        };
        let point = (1..length)
            .try_fold(initial, |point, k| {
                Some((point << 6) | continuation(i + k)?)
            })
            .filter(|&point| point <= 0x10_ffff);
        match point {
            Some(point) if length > 0 => {
                i += length;
                if point > 0xffff {
                    let point = point - 0x1_0000;
                    units.push(0xd800 | (point >> 10) as u16);
                    units.push(0xdc00 | (point & 0x3ff) as u16);
                } else {
                    units.push(point as u16);
                }
            }
            _ => {
                i += 1;
                units.push(0xfffd);
            }
        }
    }
    units
}

#[cfg(test)]
mod wire_tests;
