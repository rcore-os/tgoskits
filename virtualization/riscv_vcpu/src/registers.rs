//! Typed helpers for RISC-V register values not covered by the CSR crates.

use tock_registers::register_bitfields;

use crate::types::RiscvGuestPhysAddr;

register_bitfields! [
    usize,

    /// Hypervisor guest address translation and protection register value.
    pub HGATP [
        PPN OFFSET(0) NUMBITS(44) [],
        VMID OFFSET(44) NUMBITS(14) [],
        MODE OFFSET(60) NUMBITS(4) []
    ],

    /// Low bits supplied by `stval` for guest-page-fault physical addresses.
    pub GPF_ADDR [
        LOW OFFSET(0) NUMBITS(2) []
    ]
];

/// Encodes an `hgatp` value from a hardware mode and root page-table address.
pub fn hgatp_value(mode: usize, root_paddr: usize) -> usize {
    HGATP::MODE.val(mode).value | HGATP::PPN.val(root_paddr >> 12).value
}

/// Reconstructs a guest physical address from `htval` and `stval`.
pub fn guest_page_fault_addr(htval: usize, stval: usize) -> RiscvGuestPhysAddr {
    RiscvGuestPhysAddr::from_usize((htval << 2) | GPF_ADDR::LOW.val(stval).value)
}

/// Returns the exception delegation mask used by the per-CPU setup path.
pub const fn delegated_exception_bits() -> usize {
    exception_bit(0)
        | exception_bit(3)
        | exception_bit(8)
        | exception_bit(12)
        | exception_bit(13)
        | exception_bit(15)
        | exception_bit(2)
}

/// Returns the interrupt delegation mask used by the per-CPU setup path.
pub const fn delegated_interrupt_bits() -> usize {
    interrupt_bit(2) | interrupt_bit(6) | interrupt_bit(10)
}

const fn exception_bit(index: usize) -> usize {
    1 << index
}

const fn interrupt_bit(index: usize) -> usize {
    1 << index
}
