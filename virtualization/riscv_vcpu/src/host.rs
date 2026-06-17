//! Host callbacks required by RISC-V vCPU implementation.

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host memory operations required by RISC-V virtualization code.
#[ax_crate_interface::def_interface]
pub trait RiscvVcpuHostIf {
    /// Convert a host virtual address to host physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr;
}

pub(crate) fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    ax_crate_interface::call_interface!(RiscvVcpuHostIf::virt_to_phys(vaddr))
}
