//! The guest ABI on an aarch64 host, `x86_64`, for which uxdroid has no translator yet.
//!
//! The loader asks for the engine before it links a library for another CPU, so the refusal
//! below is the only path that runs; the other entry points exist for the loader's shape.

use crate::elf::ElfError;
use crate::table::Table;

const NO_TRANSLATOR: &str = "an arm64 host has no translator for x86_64 code yet";

pub(crate) fn engine() -> Result<(), String> {
    Err(NO_TRANSLATOR.to_string())
}

pub(crate) fn import(
    _name: &[u8],
    _tables: &[&'static Table],
    _is_data: bool,
) -> Result<Option<u64>, ElfError> {
    Err(ElfError::Unsupported("a library for another ABI"))
}

pub(crate) fn call_ifunc(_resolver: u64) -> Result<u64, String> {
    Err(NO_TRANSLATOR.to_string())
}

pub(crate) fn call_initializer(_address: u64, _environment: u64) -> Result<(), String> {
    Err(NO_TRANSLATOR.to_string())
}

pub(crate) fn call_i32(_address: u64) -> Result<i32, String> {
    Err(NO_TRANSLATOR.to_string())
}

pub(crate) mod profile {
    /// Nothing is translated, so nothing is profiled.
    pub(crate) fn library_loaded(_library: &crate::Library) {}
}
