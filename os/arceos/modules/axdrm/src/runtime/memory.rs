//! Direct-RAM address translation for DRM's owned page-cache allocations.

use ax_hal::mem::{self, MemRegionFlags, PhysAddr};
use drm_kapi::shmem::MemoryAddressOps;

pub(super) static OPS: MemoryAddressOps = MemoryAddressOps {
    virt_to_phys,
    phys_to_virt,
    totalram_pages,
};

fn is_ram(address: usize) -> bool {
    mem::phys_ram_ranges()
        .iter()
        .any(|&(base, len)| address.checked_sub(base).is_some_and(|offset| offset < len))
}

fn virt_to_phys(pointer: *const u8) -> Option<u64> {
    if pointer.is_null() {
        return None;
    }
    let address = pointer as usize;
    // KAPI's allocator owns ordinary pages in the platform direct RAM map.
    // Do not translate a user address, device aperture or arbitrary kernel
    // mapping by subtracting a platform offset from an unvalidated pointer.
    for &(physical_start, len) in mem::phys_ram_ranges() {
        let virtual_start = mem::phys_to_virt(PhysAddr::from(physical_start)).as_usize();
        if let Some(offset) = address.checked_sub(virtual_start)
            && offset < len
        {
            let physical = physical_start.checked_add(offset)?;
            if mem::phys_to_virt(PhysAddr::from(physical)).as_usize() == address {
                return Some(physical as u64);
            }
        }
    }
    None
}

fn phys_to_virt(address: u64) -> Option<*mut u8> {
    let address = usize::try_from(address).ok()?;
    is_ram(address).then(|| mem::phys_to_virt(PhysAddr::from(address)).as_usize() as *mut u8)
}

fn totalram_pages() -> usize {
    // Linux totalram_pages is managed RAM, not the current free-page count.
    // ArceOS FREE identifies allocator-managed regions even after allocation.
    mem::memory_regions()
        .filter(|region| region.flags.contains(MemRegionFlags::FREE))
        .map(|region| region.size / drm_kapi::config::PAGE_SIZE)
        .sum()
}
