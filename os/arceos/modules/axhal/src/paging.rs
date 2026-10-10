//! Page table manipulation.

use ax_alloc::{UsageKind, global_allocator};
#[doc(no_inline)]
pub use ax_cpu::paging::MappingFlags;
pub use ax_cpu::{PhysAddr, VirtAddr};
core::cfg_select! {
    all(target_arch = "aarch64", feature = "hv") => {
        type CpuPagingMeta = ax_cpu::paging::El2PagingMeta;
    }
    target_arch = "aarch64" => {
        type CpuPagingMeta = ax_cpu::paging::ArchPagingMeta;
    }
    _ => {
        pub use ax_cpu::paging::ArchPagingMeta;
    }
}
use ax_memory_addr::PAGE_SIZE_4K;
pub use page_table_generic::{
    FrameAllocator, MapConfig, PageTableEntry, PageTableOp, PagingError, PagingResult, TableMeta,
};

use crate::mem::{phys_to_virt, virt_to_phys};

/// Runtime stage-one metadata with a platform-owned AArch64 pre-make domain.
///
/// Ordinary invalidation remains local. The pre-make hardware transaction
/// covers only CPUs in the platform's Inner Shareable domain and does not
/// acknowledge owner retirement; that still requires runtime shootdown.
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
pub struct ArchPagingMeta;

#[cfg(target_arch = "aarch64")]
impl TableMeta for ArchPagingMeta {
    type P = <CpuPagingMeta as TableMeta>::P;

    const PAGE_SIZE: usize = CpuPagingMeta::PAGE_SIZE;
    const LEVEL_BITS: &'static [usize] = CpuPagingMeta::LEVEL_BITS;
    const MAX_BLOCK_LEVEL: usize = CpuPagingMeta::MAX_BLOCK_LEVEL;
    const STRICT_ADDRESS_WIDTH: bool = CpuPagingMeta::STRICT_ADDRESS_WIDTH;

    fn canonicalize_vaddr(vaddr: VirtAddr) -> VirtAddr {
        CpuPagingMeta::canonicalize_vaddr(vaddr)
    }

    fn flush(vaddr: Option<VirtAddr>) {
        CpuPagingMeta::flush(vaddr);
    }

    fn flush_batch(vaddrs: &[VirtAddr]) {
        CpuPagingMeta::flush_batch(vaddrs);
    }

    fn flush_leaf_batch(vaddrs: &[VirtAddr]) {
        CpuPagingMeta::flush_leaf_batch(vaddrs);
    }

    fn prepare_break_before_make() -> PagingResult {
        match ax_plat::mem::stage_one_tlb_domain() {
            ax_plat::mem::StageOneTlbDomain::InnerShareable => Ok(()),
            ax_plat::mem::StageOneTlbDomain::Unavailable => {
                Err(PagingError::BreakBeforeMakeDomainUnavailable)
            }
        }
    }

    fn flush_before_make(vaddr: VirtAddr, page_size: usize) -> PagingResult {
        match ax_plat::mem::stage_one_tlb_domain() {
            ax_plat::mem::StageOneTlbDomain::InnerShareable => {
                let address = (page_size == Self::PAGE_SIZE).then_some(vaddr);
                #[cfg(feature = "hv")]
                ax_cpu::mmu::El2::flush_tlb_inner_shareable(address);
                #[cfg(not(feature = "hv"))]
                ax_cpu::mmu::El1::flush_tlb_inner_shareable(address);
                Ok(())
            }
            ax_plat::mem::StageOneTlbDomain::Unavailable => {
                Err(PagingError::BreakBeforeMakeDomainUnavailable)
            }
        }
    }

    fn complete_replaced_leaf(vaddr: VirtAddr) {
        CpuPagingMeta::complete_replaced_leaf(vaddr);
    }

    fn publish_new_mapping(vaddr: VirtAddr) {
        CpuPagingMeta::publish_new_mapping(vaddr);
    }
}

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
/// Detached intermediate table frames awaiting stage-1 TLB confirmation.
pub type DeferredPageTableFrames = page_table_generic::DeferredPageTableFrames<PagingAllocator>;
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
