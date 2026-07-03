use ax_plat::mem::{MemIf, PhysAddr, RawRange, VirtAddr};

struct MemIfImpl;

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    fn phys_ram_ranges() -> &'static [RawRange] {
        crate::config::PHYS_RAM_RANGES
    }

    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        crate::config::RESERVED_RAM_RANGES
    }

    fn mmio_ranges() -> &'static [RawRange] {
        crate::config::MMIO_RANGES
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        paddr.as_usize().into()
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        vaddr.as_usize().into()
    }

    fn kernel_aspace() -> (VirtAddr, usize) {
        (
            crate::config::KERNEL_ASPACE_BASE.into(),
            crate::config::KERNEL_ASPACE_SIZE,
        )
    }
}

pub fn boot_stack_bounds(_cpu_id: usize) -> (VirtAddr, usize) {
    (0.into(), 0)
}
