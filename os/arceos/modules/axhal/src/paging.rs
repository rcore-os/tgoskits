//! Page table manipulation.

use ax_alloc::{UsageKind, global_allocator};
use ax_cpu::paging::ArchPagingMeta;
#[doc(no_inline)]
pub use ax_cpu::paging::MappingFlags;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::FrameAllocator;
pub use page_table_generic::{PagingError, PagingResult};

use crate::mem::{phys_to_virt, virt_to_phys};

/// Page-table frame allocator backed by the global kernel allocator.
#[derive(Clone, Copy)]
pub struct PagingAllocator;

impl FrameAllocator for PagingAllocator {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        self.alloc_frames(1, PAGE_SIZE_4K)
    }

    fn alloc_frames(&self, num: usize, align: usize) -> Option<PhysAddr> {
        global_allocator()
            .alloc_pages(num, align, UsageKind::PageTable)
            .map(|vaddr| virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(&self, paddr: PhysAddr) {
        self.dealloc_frames(paddr, 1, PAGE_SIZE_4K);
    }

    fn dealloc_frames(&self, paddr: PhysAddr, num: usize, _frame_size: usize) {
        global_allocator().dealloc_pages(phys_to_virt(paddr).as_usize(), num, UsageKind::PageTable);
    }

    #[inline]
    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        phys_to_virt(paddr).as_mut_ptr()
    }
}

/// The architecture-specific page table.
pub type PageTable = page_table_generic::PageTable<ArchPagingMeta, PagingAllocator>;
/// A non-owning reference to an architecture-specific page table.
pub type PageTableRef = page_table_generic::PageTableRef<ArchPagingMeta, PagingAllocator>;
