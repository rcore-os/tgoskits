//! Physical memory management.

pub use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange, pa, va,
};
pub use ax_plat::mem::{
    MemRegionFlags, PhysMemRegion, kernel_aspace, mmio_ranges, phys_ram_ranges, phys_to_virt,
    reserved_phys_ram_ranges, total_ram_size, virt_to_phys,
};
use ax_plat::mem::{check_sorted_ranges_overlap, ranges_difference};
use heapless::Vec;
use spin::LazyLock;

#[allow(unused_imports)]
const MAX_REGIONS: usize = 128;

static ALL_MEM_REGIONS: LazyLock<Vec<PhysMemRegion, MAX_REGIONS>> = LazyLock::new(|| {
    let mut all_regions = Vec::new();
    let mut push = |r: PhysMemRegion| {
        if r.size > 0 {
            all_regions.push(r).expect("too many memory regions");
        }
    };

    // Push MMIO & reserved regions
    for &(start, size) in mmio_ranges() {
        push(PhysMemRegion::new_mmio(start, size, "mmio"));
    }
    for &(start, size) in reserved_phys_ram_ranges() {
        // push(PhysMemRegion::new_reserved(start, size, "reserved"));
        push(PhysMemRegion {
            paddr: PhysAddr::from_usize(start),
            size,
            flags: MemRegionFlags::RESERVED
                | MemRegionFlags::READ
                | MemRegionFlags::WRITE
                | MemRegionFlags::EXECUTE,
            name: "reserved",
        })
    }

    let mut reserved_ranges = reserved_phys_ram_ranges()
        .iter()
        .cloned()
        .collect::<Vec<_, MAX_REGIONS>>();

    // Remove all reserved ranges from RAM ranges, and push the remaining as free memory
    reserved_ranges.sort_unstable_by_key(|&(start, _size)| start);
    ranges_difference(phys_ram_ranges(), &reserved_ranges, |(start, size)| {
        let end = start + size;
        let aligned_start = PhysAddr::from_usize(start).align_up_4k().as_usize();
        let aligned_end = PhysAddr::from_usize(end).align_down_4k().as_usize();
        if aligned_start < aligned_end {
            push(PhysMemRegion::new_ram(
                aligned_start,
                aligned_end - aligned_start,
                "free memory",
            ));
        }
    })
    .inspect_err(|(a, b)| error!("Reserved memory region {a:#x?} overlaps with {b:#x?}"))
    .unwrap();

    // Check overlapping
    all_regions.sort_unstable_by_key(|r| r.paddr);
    check_sorted_ranges_overlap(all_regions.iter().map(|r| (r.paddr.into(), r.size)))
        .inspect_err(|(a, b)| error!("Physical memory region {a:#x?} overlaps with {b:#x?}"))
        .unwrap();

    all_regions
});

/// Returns an iterator over all physical memory regions.
pub fn memory_regions() -> impl Iterator<Item = PhysMemRegion> {
    ALL_MEM_REGIONS.iter().cloned()
}

pub fn boot_stack_bounds(cpu_id: usize) -> (VirtAddr, usize) {
    #[cfg(any(test, feature = "host-test"))]
    {
        let _ = cpu_id;
        (va!(0), 0)
    }

    #[cfg(not(any(test, feature = "host-test")))]
    axplat_dyn::boot_stack_bounds(cpu_id)
}

/// Fills the `.bss` section with zeros.
///
/// It requires the symbols `_sbss` and `_ebss` to be defined in the linker script.
///
/// # Safety
///
/// This function is unsafe because it writes `.bss` section directly.
pub unsafe fn clear_bss() {
    unsafe {
        core::slice::from_raw_parts_mut(
            _sbss as *mut u8,
            (_ebss as *mut u8).offset_from_unsigned(_sbss as *mut u8),
        )
        .fill(0);
    }
}

#[allow(dead_code)]
unsafe extern "C" {
    fn _stext();
    fn _etext();
    fn _srodata();
    fn _erodata();
    fn _sdata();
    fn _edata();
    fn _sbss();
    fn _ebss();
    fn _skernel();
    fn _ekernel();
    fn boot_stack();
    fn boot_stack_top();
}
