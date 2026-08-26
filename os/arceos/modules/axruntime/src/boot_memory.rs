//! Early global allocator construction.

pub(crate) fn init_allocator() {
    use ax_hal::mem::{MemRegionFlags, memory_regions, phys_to_virt};

    info!("Initialize global memory allocator...");
    info!("  use {} allocator.", ax_alloc::global_allocator().name());

    // The page allocator (which backs user-space page population via
    // `alloc_pages`) is initialized from a single contiguous region by
    // `global_init`; every other free region is handed to the byte/heap
    // allocator by `global_add_memory` (the bitmap page allocator does not
    // support `add_memory`). So the region chosen for `global_init` *is* the
    // entire pool available for user memory.
    //
    // Pick the LARGEST free region for the page allocator. Platforms with a
    // single contiguous RAM region (x86/aarch64/riscv64 qemu-virt) are
    // unaffected (largest == the only region). Platforms with disjoint regions
    // (loongarch64 qemu-virt: a small ~248 MB low region below the MMIO hole
    // plus the multi-GB high region at 0x8000_0000) previously picked the small
    // low region — the "first free region after .bss" heuristic — which capped
    // all user allocations at ~248 MB regardless of total RAM, OOM'ing large
    // workloads (e.g. the gradle build JVM) even with gigabytes free.
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
            ax_alloc::global_add_memory(phys_to_virt(region.paddr).as_usize(), region.size)
                .expect("add heap memory region failed");
        }
    }
}
