use axplat::mem::{MemIf, PhysAddr, RawRange, VirtAddr, pa, va};

use crate::config::devices::MMIO_RANGES;
use crate::config::plat::{
    HIGH_MEMORY_BASE, LOW_MEMORY_BASE, LOW_MEMORY_SIZE, PHYS_BOOT_OFFSET, PHYS_MEMORY_SIZE,
    PHYS_VIRT_OFFSET,
};

struct MemIfImpl;

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    /// Returns all physical memory (RAM) ranges on the platform.
    ///
    /// All memory ranges except reserved ranges (including the kernel loaded
    /// range) are free for allocation.
    fn phys_ram_ranges() -> &'static [RawRange] {
        const HIGH_MEMORY_SIZE: usize = PHYS_MEMORY_SIZE.saturating_sub(LOW_MEMORY_SIZE);

        if HIGH_MEMORY_SIZE == 0 {
            &[(LOW_MEMORY_BASE, PHYS_MEMORY_SIZE)]
        } else {
            &[
                (LOW_MEMORY_BASE, LOW_MEMORY_SIZE),
                (HIGH_MEMORY_BASE, HIGH_MEMORY_SIZE),
            ]
        }
    }

    /// Returns all reserved physical memory ranges on the platform.
    ///
    /// Reserved memory can be contained in [`phys_ram_ranges`], they are not
    /// allocatable but should be mapped to kernel's address space.
    ///
    /// Note that the ranges returned should not include the range where the
    /// kernel is loaded.
    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        &[(0, 0x200000)] // boot_info + fdt
    }

    /// Returns all device memory (MMIO) ranges on the platform.
    fn mmio_ranges() -> &'static [RawRange] {
        &MMIO_RANGES
    }

    /// Translates a physical address to a virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        va!(paddr.as_usize() + PHYS_VIRT_OFFSET)
    }

    /// Translates a virtual address to a physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        let vaddr = vaddr.as_usize();
        if vaddr & 0xffff_0000_0000_0000 == PHYS_BOOT_OFFSET {
            pa!(vaddr - PHYS_BOOT_OFFSET)
        } else {
            pa!(vaddr - PHYS_VIRT_OFFSET)
        }
    }

    /// Returns the kernel address space base virtual address and size.
    fn kernel_aspace() -> (VirtAddr, usize) {
        (
            va!(crate::config::plat::KERNEL_ASPACE_BASE),
            crate::config::plat::KERNEL_ASPACE_SIZE,
        )
    }
}
