use ax_alloc::{MemoryZone, PageRequest, UsageKind, global_allocator};
use ax_hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageSize, PageTable},
};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr};

use super::Backend;

fn alloc_frame(zeroed: bool) -> Option<PhysAddr> {
    let vaddr = VirtAddr::from(
        global_allocator()
            .allocate_pages_raw(
                PageRequest {
                    count: 1,
                    align: PAGE_SIZE_4K,
                    zone: MemoryZone::Normal,
                },
                UsageKind::VirtMem,
            )
            .ok()?,
    );
    if zeroed {
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, PAGE_SIZE_4K) };
    }
    let paddr = virt_to_phys(vaddr);
    Some(paddr)
}

pub(super) fn dealloc_frame(frame: PhysAddr) {
    let vaddr = phys_to_virt(frame);
    // SAFETY: allocated mappings call this exactly once for a frame returned
    // by alloc_frame with the same single-page request and usage.
    unsafe {
        global_allocator().deallocate_pages_raw(vaddr.as_usize(), 1, UsageKind::VirtMem);
    }
}

impl Backend {
    /// Creates a new allocation mapping backend.
    pub const fn new_alloc(populate: bool) -> Self {
        Self::Alloc { populate }
    }

    pub(crate) fn map_alloc(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        debug!(
            "map_alloc: [{:#x}, {:#x}) {:?} (populate={})",
            start, end, flags, populate
        );
        if populate {
            // allocate all possible physical frames for populated mapping.
            let mut mapped_pages = 0;
            for addr in PageIter4K::new(start, end)
                .expect("prepared allocation range must be 4-KiB aligned")
            {
                if let Some(frame) = alloc_frame(true) {
                    if pt
                        .cursor()
                        .map(addr, frame, PageSize::Size4K, flags)
                        .is_err()
                    {
                        dealloc_frame(frame);
                        rollback_alloc_mapping(start, mapped_pages, pt);
                        return false;
                    }
                    mapped_pages += 1;
                    // TLB flush on map is unnecessary, as there are no outdated mappings.
                } else {
                    rollback_alloc_mapping(start, mapped_pages, pt);
                    return false;
                }
            }
            true
        } else {
            // Map to a empty entry for on-demand mapping.
            let flags = MappingFlags::empty();
            pt.cursor()
                .map_region(start, |_| 0.into(), size, flags, false)
                .is_ok()
        }
    }

    pub(crate) fn unmap_alloc(
        &self,
        start: VirtAddr,
        size: usize,
        pt: &mut PageTable,
        _populate: bool,
    ) -> bool {
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        debug!("unmap_alloc: [{:#x}, {:#x})", start, end);
        for addr in PageIter4K::new(start, end).expect("prepared unmap range must be 4-KiB aligned")
        {
            if pt
                .query(addr)
                .is_ok_and(|(_, _, page_size)| page_size.is_huge())
            {
                return false;
            }
        }
        for addr in PageIter4K::new(start, end).expect("prepared unmap range must be 4-KiB aligned")
        {
            match pt.cursor().unmap(addr) {
                Ok((frame, _, page_size)) => {
                    debug_assert_eq!(page_size, PageSize::Size4K);
                    // TLB flush is handled automatically when cursor is dropped.
                    dealloc_frame(frame);
                }
                Err(ax_hal::paging::PagingError::NotMapped) => {}
                Err(_) => return false,
            }
        }
        true
    }

    pub(crate) fn handle_page_fault_alloc(
        &self,
        vaddr: VirtAddr,
        orig_flags: MappingFlags,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        if populate {
            false // Populated mappings should not trigger page faults.
        } else if let Some(frame) = alloc_frame(true) {
            // Allocate a physical frame lazily and map it to the fault address.
            // `vaddr` does not need to be aligned. It will be automatically
            // aligned during `pt.cursor().remap` regardless of the page size.
            if pt.cursor().remap(vaddr, frame, orig_flags).is_ok() {
                true
            } else {
                dealloc_frame(frame);
                false
            }
        } else {
            false
        }
    }
}

fn rollback_alloc_mapping(start: VirtAddr, mapped_pages: usize, pt: &mut PageTable) {
    let bytes = mapped_pages
        .checked_mul(PAGE_SIZE_4K)
        .expect("mapped page count must fit in an address range");
    let end = start
        .checked_add(bytes)
        .expect("mapped rollback range must not overflow");
    for addr in PageIter4K::new(start, end).expect("mapped rollback range must be aligned") {
        if let Ok((frame, _, page_size)) = pt.cursor().unmap(addr) {
            debug_assert_eq!(page_size, PageSize::Size4K);
            dealloc_frame(frame);
        }
    }
}
