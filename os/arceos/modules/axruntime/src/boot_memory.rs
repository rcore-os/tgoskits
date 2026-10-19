//! Early global allocator construction.

pub(crate) fn init_allocator() {
    use ax_hal::mem::{MemRegionFlags, memory_regions, phys_to_virt};

    info!("Initialize global memory allocator...");
    info!("  use {} allocator.", ax_alloc::global_allocator().name());

    // Seed the allocator with the largest region. Both page and slab allocations
    // can subsequently use every added section; compact insertion preserves
    // page-aligned fragments reclaimed from earlier boot stages.
    let mut max_region_size = 0;
    let mut max_region_paddr = 0.into();

    for region in memory_regions() {
        if region.flags.contains(MemRegionFlags::FREE) && region.size > max_region_size {
            max_region_size = region.size;
            max_region_paddr = region.paddr;
        }
    }

    for region in memory_regions() {
        if region.flags.contains(MemRegionFlags::FREE) && region.paddr == max_region_paddr {
            ax_alloc::global_init(phys_to_virt(region.paddr).as_usize(), region.size)
                .expect("initialize global allocator failed");
            break;
        }
    }

    for region in memory_regions() {
        if region.flags.contains(MemRegionFlags::FREE) && region.paddr != max_region_paddr {
            // SAFETY: the platform lists exclusive Free RAM before any user of
            // the global allocator runs. These regions do not overlap the seed.
            unsafe {
                ax_alloc::global_add_memory_compact(
                    phys_to_virt(region.paddr).as_usize(),
                    region.size,
                )
            }
            .expect("add heap memory region failed");
        }
    }
}
