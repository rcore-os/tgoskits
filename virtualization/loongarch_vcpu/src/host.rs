//! Host callbacks required by LoongArch vCPU implementation.

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host memory operations required by LoongArch virtualization code.
#[ax_crate_interface::def_interface]
pub trait LoongArchVcpuHostIf {
    /// Convert a host virtual address to host physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr;
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::virt_to_phys(vaddr))
}
