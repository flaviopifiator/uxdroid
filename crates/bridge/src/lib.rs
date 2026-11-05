//! Loads bionic-linked shared objects into a glibc process.
//!
//! x86_64 and aarch64 glibc hosts are supported: the shims encode each ABI pair, bionic's and
//! glibc's for the same CPU. Only the x86_64 host has a translator, for `arm64-v8a` guests.
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux",
    target_env = "gnu"
))]

#[cfg(target_arch = "aarch64")]
pub mod bionic_tls;
pub mod elf;
#[cfg(target_arch = "x86_64")]
pub mod guest;
#[cfg(target_arch = "aarch64")]
#[path = "no_guest.rs"]
mod guest;
mod image;
#[macro_use]
mod shims;
mod liblog;
mod loader;
pub mod output;
mod paths;
pub mod table;

pub use loader::{Error, Library, is_guest_address, load};
pub use shims::{HostBinding, host_binding};
