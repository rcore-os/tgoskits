use core::ptr::NonNull;

use ax_plat::mem::{MemIf, PhysAddr, RawRange, VirtAddr, pa, va};
use spin::Once;

use crate::config::{
    devices::MMIO_RANGES,
    plat::{KERNEL_BASE_PADDR, PHYS_MEMORY_BASE, PHYS_MEMORY_SIZE, PHYS_VIRT_OFFSET},
};

static DTB_ADDR: Once<usize> = Once::new();

/// Store the DTB physical address for later memory discovery.
pub fn init_dtb(addr: usize) {
    DTB_ADDR.call_once(|| addr);
}

/// Parse the device tree to find the total physical memory size.
/// Returns `None` if the DTB is not available or has no memory node.
fn memory_size_from_dtb() -> Option<usize> {
    let dtb_paddr = *DTB_ADDR.get()?;
    // At this point in boot, the linear mapping is already set up so we
    // can convert the physical DTB address to a virtual pointer.
    let dtb_vaddr = dtb_paddr + PHYS_VIRT_OFFSET;
    let ptr = NonNull::new(dtb_vaddr as *mut u8)?;
    let fdt = fdt_parser::Fdt::from_ptr(ptr).ok()?;

    // Find the memory node and sum all memory regions.
    for node in fdt.all_nodes() {
        if node.name().starts_with("memory") {
            if let Some(reg) = node.reg() {
                let mut total = 0usize;
                for region in reg {
                    if let Some(size) = region.size {
                        total += size;
                    }
                }
                if total > 0 {
                    return Some(total);
                }
            }
        }
    }
    None
}

struct MemIfImpl;

#[impl_plat_interface]
impl MemIf for MemIfImpl {
    fn phys_ram_ranges() -> &'static [RawRange] {
        static RAM_RANGES: Once<[RawRange; 1]> = Once::new();
        RAM_RANGES.call_once(|| {
            let mem_size = memory_size_from_dtb().unwrap_or(PHYS_MEMORY_SIZE);
            let mem_end = PHYS_MEMORY_BASE + mem_size;
            let usable = mem_end - KERNEL_BASE_PADDR;
            [(KERNEL_BASE_PADDR, usable)]
        })
    }

    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        &[]
    }

    fn mmio_ranges() -> &'static [RawRange] {
        &MMIO_RANGES
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        va!(paddr.as_usize() + PHYS_VIRT_OFFSET)
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        pa!(vaddr.as_usize() - PHYS_VIRT_OFFSET)
    }

    fn kernel_aspace() -> (VirtAddr, usize) {
        (
            va!(crate::config::plat::KERNEL_ASPACE_BASE),
            crate::config::plat::KERNEL_ASPACE_SIZE,
        )
    }
}
