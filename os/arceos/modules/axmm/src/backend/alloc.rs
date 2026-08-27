use ax_alloc::{UsageKind, global_allocator};
use ax_hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageTable, PagingError},
};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr};

use super::Backend;
use crate::tlb::TlbGather;

fn alloc_frame(zeroed: bool) -> Option<PhysAddr> {
    let vaddr = VirtAddr::from(
        global_allocator()
            .alloc_pages(1, PAGE_SIZE_4K, UsageKind::VirtMem)
            .ok()?,
    );
    if zeroed {
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, PAGE_SIZE_4K) };
    }
    let paddr = virt_to_phys(vaddr);
    Some(paddr)
}

pub(crate) fn dealloc_frame(frame: PhysAddr) {
    let vaddr = phys_to_virt(frame);
    global_allocator().dealloc_pages(vaddr.as_usize(), 1, UsageKind::VirtMem);
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
        gather: &mut TlbGather,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        debug!(
            "map_alloc: [{:#x}, {:#x}) {:?} (populate={})",
            start,
            start + size,
            flags,
            populate
        );
        if populate {
            let mut populate = PageTablePopulate {
                page_table: pt,
                flags,
                gather,
            };
            populate_pages(&mut populate, start, size)
        } else {
            // Map to a empty entry for on-demand mapping.
            let flags = MappingFlags::empty();
            pt.map_region(start, |_| 0.into(), size, flags, false)
                .is_ok()
        }
    }

    pub(crate) fn unmap_alloc(
        &self,
        start: VirtAddr,
        size: usize,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        _populate: bool,
    ) -> bool {
        debug!("unmap_alloc: [{:#x}, {:#x})", start, start + size);
        let mut mapped = alloc::vec::Vec::new();
        for addr in PageIter4K::new(start, start + size).unwrap() {
            match pt.query(addr) {
                Ok((frame, _, PAGE_SIZE_4K)) => mapped.push((addr, frame)),
                Ok(_) => return false,
                Err(PagingError::NotMapped) => {}
                Err(_) => return false,
            }
        }
        for (addr, frame) in mapped {
            pt.unmap_page(addr)
                .expect("a preflighted allocated page must remain mapped under the aspace lock");
            gather.defer_frame(frame);
        }
        gather.invalidate(start, size);
        true
    }

    pub(crate) fn handle_page_fault_alloc(
        &self,
        vaddr: VirtAddr,
        orig_flags: MappingFlags,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        populate: bool,
    ) -> bool {
        if populate {
            false // Populated mappings should not trigger page faults.
        } else {
            // Allocate a physical frame lazily and map it to the fault address.
            remap_frame_or_dealloc(
                alloc_frame(true),
                |frame| pt.remap_page(vaddr, frame, orig_flags).is_ok(),
                dealloc_frame,
                |_| gather.invalidate(vaddr.align_down_4k(), PAGE_SIZE_4K),
            )
        }
    }
}

fn remap_frame_or_dealloc(
    frame: Option<PhysAddr>,
    remap_frame: impl FnOnce(PhysAddr) -> bool,
    dealloc_frame: impl FnOnce(PhysAddr),
    publish_frame: impl FnOnce(PhysAddr),
) -> bool {
    let Some(frame) = frame else {
        return false;
    };
    if remap_frame(frame) {
        publish_frame(frame);
        true
    } else {
        dealloc_frame(frame);
        false
    }
}

trait PopulatePageOps {
    fn alloc_frame(&mut self) -> Option<PhysAddr>;

    fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool;

    fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr>;

    /// Releases a frame that was never reachable through a published PTE.
    fn release_unmapped_frame(&mut self, frame: PhysAddr);

    /// Retains a frame removed from a PTE until shootdown confirmation.
    fn defer_unmapped_frame(&mut self, frame: PhysAddr);
}

struct PageTablePopulate<'a> {
    page_table: &'a mut PageTable,
    flags: MappingFlags,
    gather: &'a mut TlbGather,
}

impl PopulatePageOps for PageTablePopulate<'_> {
    fn alloc_frame(&mut self) -> Option<PhysAddr> {
        alloc_frame(true)
    }

    fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool {
        self.page_table
            .map_page(addr, frame, PAGE_SIZE_4K, self.flags)
            .is_ok()
    }

    fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr> {
        let (frame, _, page_size) = self.page_table.unmap_page(addr).ok()?;
        assert_eq!(
            page_size, PAGE_SIZE_4K,
            "a populated 4K mapping must be rolled back as a 4K page"
        );
        Some(frame)
    }

    fn release_unmapped_frame(&mut self, frame: PhysAddr) {
        dealloc_frame(frame);
    }

    fn defer_unmapped_frame(&mut self, frame: PhysAddr) {
        self.gather.defer_frame(frame);
    }
}

fn populate_pages(ops: &mut impl PopulatePageOps, start: VirtAddr, size: usize) -> bool {
    for addr in PageIter4K::new(start, start + size).unwrap() {
        let Some(frame) = ops.alloc_frame() else {
            rollback_populated_pages(ops, start, addr);
            return false;
        };
        if !ops.map_frame(addr, frame) {
            ops.release_unmapped_frame(frame);
            rollback_populated_pages(ops, start, addr);
            return false;
        }
    }
    true
}

fn rollback_populated_pages(ops: &mut impl PopulatePageOps, start: VirtAddr, mapped_end: VirtAddr) {
    for addr in PageIter4K::new(start, mapped_end).unwrap() {
        let frame = ops
            .unmap_frame(addr)
            .expect("a page mapped by the current populate operation must remain mapped");
        ops.defer_unmapped_frame(frame);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::Cell;

    use super::*;

    const START: usize = 0x4000_0000;

    #[test]
    fn rolls_back_current_and_mapped_frames_when_map_fails() {
        let start = VirtAddr::from(START);
        let mut ops = MockPopulatePageOps::with_map_failure(start + PAGE_SIZE_4K);

        assert!(!populate_pages(&mut ops, start, 3 * PAGE_SIZE_4K));
        assert!(ops.mapped.is_empty());
        assert_eq!(ops.deallocated.len(), 2);
        assert!(ops.deallocated.contains(&PhysAddr::from(PAGE_SIZE_4K)));
        assert!(ops.deallocated.contains(&PhysAddr::from(2 * PAGE_SIZE_4K)));
    }

    #[test]
    fn rolls_back_mapped_frames_when_allocation_fails() {
        let start = VirtAddr::from(START);
        let mut ops = MockPopulatePageOps::with_allocation_limit(1);

        assert!(!populate_pages(&mut ops, start, 3 * PAGE_SIZE_4K));
        assert!(ops.mapped.is_empty());
        assert_eq!(ops.deallocated, [PhysAddr::from(PAGE_SIZE_4K)]);
    }

    #[test]
    fn keeps_lazy_frame_when_remap_succeeds() {
        let frame = PhysAddr::from(PAGE_SIZE_4K);
        let deallocated = Cell::new(None);
        let published = Cell::new(None);

        assert!(remap_frame_or_dealloc(
            Some(frame),
            |_| true,
            |frame| deallocated.set(Some(frame)),
            |frame| published.set(Some(frame)),
        ));
        assert_eq!(deallocated.get(), None);
        assert_eq!(published.get(), Some(frame));
    }

    #[test]
    fn deallocates_lazy_frame_when_remap_fails() {
        let frame = PhysAddr::from(PAGE_SIZE_4K);
        let deallocated = Cell::new(None);

        assert!(!remap_frame_or_dealloc(
            Some(frame),
            |_| false,
            |frame| deallocated.set(Some(frame)),
            |_| unreachable!("failed remap must not publish its frame"),
        ));
        assert_eq!(deallocated.get(), Some(frame));
    }

    struct MockPopulatePageOps {
        allocations: usize,
        allocation_limit: Option<usize>,
        map_failure: Option<VirtAddr>,
        mapped: Vec<(VirtAddr, PhysAddr)>,
        deallocated: Vec<PhysAddr>,
    }

    impl MockPopulatePageOps {
        fn with_map_failure(addr: VirtAddr) -> Self {
            Self {
                allocations: 0,
                allocation_limit: None,
                map_failure: Some(addr),
                mapped: Vec::new(),
                deallocated: Vec::new(),
            }
        }

        fn with_allocation_limit(limit: usize) -> Self {
            Self {
                allocations: 0,
                allocation_limit: Some(limit),
                map_failure: None,
                mapped: Vec::new(),
                deallocated: Vec::new(),
            }
        }
    }

    impl PopulatePageOps for MockPopulatePageOps {
        fn alloc_frame(&mut self) -> Option<PhysAddr> {
            if self.allocation_limit == Some(self.allocations) {
                return None;
            }
            self.allocations += 1;
            Some(PhysAddr::from(self.allocations * PAGE_SIZE_4K))
        }

        fn map_frame(&mut self, addr: VirtAddr, frame: PhysAddr) -> bool {
            if self.map_failure == Some(addr) {
                return false;
            }
            self.mapped.push((addr, frame));
            true
        }

        fn unmap_frame(&mut self, addr: VirtAddr) -> Option<PhysAddr> {
            let index = self
                .mapped
                .iter()
                .position(|(mapped_addr, _)| *mapped_addr == addr)?;
            Some(self.mapped.remove(index).1)
        }

        fn release_unmapped_frame(&mut self, frame: PhysAddr) {
            self.deallocated.push(frame);
        }

        fn defer_unmapped_frame(&mut self, frame: PhysAddr) {
            self.deallocated.push(frame);
        }
    }
}
