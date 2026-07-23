//! Page table manipulation.

use ax_alloc::{MemoryZone, PageRequest, UsageKind, global_allocator};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};
use ax_page_table::stage1::PageFrameProvider;
#[doc(no_inline)]
pub use ax_page_table::stage1::{MappingFlags, PageSize, PagingError, PagingResult};
#[cfg(not(target_arch = "aarch64"))]
use ax_page_table::stage1::{TlbInvalidator, TlbScope};

use crate::mem::{phys_to_virt, virt_to_phys};

/// Validates that this runtime can invalidate translations on every online CPU.
///
/// AArch64 uses hardware broadcast. Architectures with local-only invalidation
/// require the `ipi` feature when more than one CPU is online.
pub fn validate_smp_invalidation() {
    if crate::cpu_num() <= 1 {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    let available = ax_page_table::stage1::smp_invalidation_available::<
        ax_page_table::stage1::x86_64::X64PagingMetaData<RuntimeTlbInvalidator>,
    >(false);
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    let available = ax_page_table::stage1::smp_invalidation_available::<
        ax_page_table::stage1::riscv::Sv39MetaData<VirtAddr, RuntimeTlbInvalidator>,
    >(false);
    #[cfg(target_arch = "aarch64")]
    let available = ax_page_table::stage1::smp_invalidation_available::<
        ax_page_table::stage1::aarch64::A64PagingMetaData,
    >(cfg!(feature = "ipi"));
    #[cfg(target_arch = "loongarch64")]
    let available = ax_page_table::stage1::smp_invalidation_available::<
        ax_page_table::stage1::loongarch64::LA64MetaData<RuntimeTlbInvalidator>,
    >(false);
    assert!(
        available,
        "SMP paging requires hardware broadcast or the ax-hal `ipi` feature"
    );
}

/// Runtime invalidation for architectures that require explicit remote IPIs.
#[cfg(not(target_arch = "aarch64"))]
pub struct RuntimeTlbInvalidator;

#[cfg(not(target_arch = "aarch64"))]
impl TlbInvalidator<VirtAddr> for RuntimeTlbInvalidator {
    const SCOPE: TlbScope = if cfg!(feature = "ipi") {
        TlbScope::RemoteIpi
    } else {
        TlbScope::Local
    };

    fn invalidate(vaddr: Option<VirtAddr>) {
        if let Some(vaddr) = vaddr {
            crate::cache::flush_tlb_range_all_cpus(vaddr, PAGE_SIZE_4K);
        } else {
            crate::cache::flush_tlb_all_cpus();
        }
    }

    fn invalidate_list(vaddrs: &[VirtAddr]) {
        crate::cache::flush_tlb_list_all_cpus(vaddrs);
    }
}

/// Runtime frame source for stage-1 page tables.
#[derive(Clone, Copy, Default)]
pub struct PagingHandlerImpl;

impl PageFrameProvider for PagingHandlerImpl {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        self.alloc_frames(1, PAGE_SIZE_4K)
    }

    fn alloc_frames(&self, num: usize, align: usize) -> Option<PhysAddr> {
        global_allocator()
            .allocate_pages_raw(
                PageRequest {
                    count: num,
                    align,
                    zone: MemoryZone::Normal,
                },
                UsageKind::PageTable,
            )
            .map(|vaddr| virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(&self, paddr: PhysAddr) {
        self.dealloc_frames(paddr, 1)
    }

    fn dealloc_frames(&self, paddr: PhysAddr, num: usize) {
        // SAFETY: PageFrameProvider returns only frame ranges allocated by the
        // matching method on this provider, with the original frame count.
        unsafe {
            global_allocator().deallocate_pages_raw(
                phys_to_virt(paddr).as_usize(),
                PageRequest {
                    count: num,
                    align: PAGE_SIZE_4K,
                    zone: MemoryZone::Normal,
                },
                UsageKind::PageTable,
            );
        }
    }

    #[inline]
    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
        phys_to_virt(paddr)
    }
}

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        /// The architecture-specific page table.
        pub type PageTable = ax_page_table::stage1::x86_64::X64PageTable<PagingHandlerImpl, RuntimeTlbInvalidator>;
        /// The architecture-specific page table cursor.
        pub type PageTableCursor<'a> = ax_page_table::stage1::x86_64::X64PageTableCursor<'a, PagingHandlerImpl, RuntimeTlbInvalidator>;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        /// The architecture-specific page table.
        pub type PageTable = ax_page_table::stage1::riscv::Sv39PageTable<PagingHandlerImpl, RuntimeTlbInvalidator>;
        /// The architecture-specific page table cursor.
        pub type PageTableCursor<'a> = ax_page_table::stage1::riscv::Sv39PageTableCursor<'a, PagingHandlerImpl, RuntimeTlbInvalidator>;
    } else if #[cfg(target_arch = "aarch64")]{
        /// The architecture-specific page table.
        pub type PageTable = ax_page_table::stage1::aarch64::A64PageTable<PagingHandlerImpl>;
        /// The architecture-specific page table cursor.
        pub type PageTableCursor<'a> = ax_page_table::stage1::aarch64::A64PageTableCursor<'a, PagingHandlerImpl>;
    } else if #[cfg(target_arch = "loongarch64")] {
        /// The architecture-specific page table.
        pub type PageTable = ax_page_table::stage1::loongarch64::LA64PageTable<PagingHandlerImpl, RuntimeTlbInvalidator>;
        /// The architecture-specific page table cursor.
        pub type PageTableCursor<'a> = ax_page_table::stage1::loongarch64::LA64PageTableCursor<'a, PagingHandlerImpl, RuntimeTlbInvalidator>;
    }
}
