//! Page table manipulation.

use ax_alloc::{UsageKind, global_allocator};
use ax_cpu::paging::ArchPagingMeta;
#[doc(no_inline)]
pub use ax_cpu::paging::MappingFlags;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::FrameAllocator;
pub use page_table_generic::{PageTableEntry, PagingError, PagingResult};

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
/// Allocation-free plan for preparing one architecture-specific page-table leaf.
pub type PageTableMapPlan = page_table_generic::PageTableMapPlan<ArchPagingMeta, PagingAllocator>;
/// Move-only, preallocated page-table suffix for one exact leaf.
pub type PageTableMapDeposit =
    page_table_generic::PageTableMapDeposit<ArchPagingMeta, PagingAllocator>;
/// Recoverable apply error that returns an uninstalled [`PageTableMapDeposit`].
pub type PageTableMapApplyError =
    page_table_generic::PageTableMapApplyError<ArchPagingMeta, PagingAllocator>;
/// Immutable identity of one architecture-specific occupied leaf.
pub type PageTableLeafPlan = page_table_generic::PageTableLeafPlan<ArchPagingMeta>;
/// Allocation-free plan for one architecture-specific PTE relocation.
pub type PageTableMovePlan = page_table_generic::PageTableMovePlan<ArchPagingMeta, PagingAllocator>;
/// Move-only ownership of an empty, detached page-table suffix.
pub type PageTablePathDeposit =
    page_table_generic::PageTablePathDeposit<ArchPagingMeta, PagingAllocator>;
/// Recoverable path-publication failure.
pub type PageTablePathApplyError =
    page_table_generic::PageTablePathApplyError<ArchPagingMeta, PagingAllocator>;
/// A pre-zeroed child table bound to one architecture-specific huge leaf.
pub type HugeSplitDeposit = page_table_generic::HugeSplitDeposit<ArchPagingMeta, PagingAllocator>;
/// Recoverable apply error that returns an uninstalled [`HugeSplitDeposit`].
pub type HugeSplitApplyError =
    page_table_generic::HugeSplitApplyError<ArchPagingMeta, PagingAllocator>;
/// Receipt for a child table installed by consuming a [`HugeSplitDeposit`].
pub type InstalledHugeSplit = page_table_generic::InstalledHugeSplit<ArchPagingMeta>;
