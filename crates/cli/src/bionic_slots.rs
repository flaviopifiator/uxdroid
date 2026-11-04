//! The TLS this executable reserves for bionic's thread pointer slots on AArch64
//! (`uxdroid_bridge::bionic_tls`): 48 bytes, 16-aligned, the first of the executable's TLS, so
//! glibc places them right after its 16-byte thread control block. The local-exec access below is
//! only valid in an executable, which is why the reservation lives here and not in the bridge.

std::arch::global_asm!(
    ".section .tdata.uxdroid_bionic_slots, \"awT\", %progbits",
    ".p2align 4",
    ".type uxdroid_bionic_slots, %object",
    "uxdroid_bionic_slots:",
    ".zero 48",
    ".size uxdroid_bionic_slots, 48",
    ".text",
    ".p2align 2",
    ".type uxdroid_bionic_slots_address, %function",
    "uxdroid_bionic_slots_address:",
    "mrs x0, tpidr_el0",
    "add x0, x0, #:tprel_hi12:uxdroid_bionic_slots, lsl #12",
    "add x0, x0, #:tprel_lo12_nc:uxdroid_bionic_slots",
    "ret",
    ".size uxdroid_bionic_slots_address, . - uxdroid_bionic_slots_address",
);

unsafe extern "C" {
    fn uxdroid_bionic_slots_address() -> usize;
}

/// Hands the reservation to the bridge, which checks where it landed.
pub fn reserve() -> Result<(), String> {
    uxdroid_bridge::bionic_tls::reserve(uxdroid_bionic_slots_address)
}
