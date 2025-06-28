use axplat::mem::{MemIf, PhysAddr, RawRange, VirtAddr};

struct MemIfImpl;

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    /// Returns all physical memory (RAM) ranges on the platform.
    ///
    /// All memory ranges except reserved ranges (including the kernel loaded
    /// range) are free for allocation.
    fn phys_ram_ranges() -> &'static [RawRange] {
        todo!()
    }

    /// Returns all reserved physical memory ranges on the platform.
    ///
    /// Reserved memory can be contained in [`phys_ram_ranges`], they are not
    /// allocatable but should be mapped to kernel's address space.
    ///
    /// Note that the ranges returned should not include the range where the
    /// kernel is loaded.
    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        todo!()
    }

    /// Returns all device memory (MMIO) ranges on the platform.
    fn mmio_ranges() -> &'static [RawRange] {
        todo!()
    }

    /// Translates a physical address to a virtual address.
    ///
    /// It is just an easy way to access physical memory when virtual memory
    /// is enabled. The mapping may not be unique, there can be multiple `vaddr`s
    /// mapped to that `paddr`.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        todo!()
    }

    /// Translates a virtual address to a physical address.
    ///
    /// It is a reverse operation of [`phys_to_virt`]. It requires that the
    /// `vaddr` must be available through the [`phys_to_virt`] translation.
    /// It **cannot** be used to translate arbitrary virtual addresses.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        todo!()
    }
}
