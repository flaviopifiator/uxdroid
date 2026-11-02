//! Loads bionic-linked shared objects into a glibc process.
//!
//! Only x86_64 glibc hosts are supported, because the shims encode that ABI pair.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

pub mod elf;
mod image;
mod loader;
mod shims;

pub use loader::{Error, Library, load};
