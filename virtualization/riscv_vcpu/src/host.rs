//! Host callbacks required by RISC-V vCPU implementation.

use crate::types::{RiscvHostPhysAddr, RiscvHostVirtAddr};

/// Host memory operations required by RISC-V virtualization code.
pub trait RiscvHostOps {
    /// Convert a host virtual address to host physical address.
    fn virt_to_phys(vaddr: RiscvHostVirtAddr) -> RiscvHostPhysAddr;
}
