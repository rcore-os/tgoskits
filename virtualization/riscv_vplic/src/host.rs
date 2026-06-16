//! Host callbacks required by RISC-V virtual PLIC.

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host memory operations required by RISC-V virtual PLIC.
#[ax_crate_interface::def_interface]
pub trait RiscvVplicHostIf {
    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;
}

pub(crate) fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    ax_crate_interface::call_interface!(RiscvVplicHostIf::phys_to_virt(paddr))
}
