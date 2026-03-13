//! ARMv7-A specific page table structures.

use core::arch::asm;

use page_table_entry::arm::A32PTE;

use crate::{PageTable32, PageTable32Cursor, PagingMetaData};

/// Metadata of ARMv7-A page tables.
pub struct A32PagingMetaData;

impl PagingMetaData for A32PagingMetaData {
    const LEVELS: usize = 2; // ARMv7-A uses 2-level page tables
    const PA_MAX_BITS: usize = 32;
    const VA_MAX_BITS: usize = 32;
    type VirtAddr = memory_addr::VirtAddr;

    fn vaddr_is_valid(_vaddr: usize) -> bool {
        // All 32-bit addresses are valid
        //     vaddr < 0xFFFF_FFFF
        true
    }

    #[inline]
    fn flush_tlb(vaddr: Option<memory_addr::VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                // Invalidate unified TLB entry by MVA
                asm!(
                    "mcr p15, 0, {0}, c8, c7, 1", // TLBIMVA
                    in(reg) vaddr.as_usize(),
                );
            } else {
                // Invalidate entire unified TLB
                let zero: usize = 0;
                asm!(
                    "mcr p15, 0, {0}, c8, c7, 0", // TLBIALL
                    in(reg) zero,
                );
            }
            // Data Synchronization Barrier
            asm!("dsb");
            // Instruction Synchronization Barrier
            asm!("isb");
        }
    }
}

/// ARMv7-A Short-descriptor translation table.
pub type A32PageTable<H> = PageTable32<A32PagingMetaData, A32PTE, H>;
/// ARMv7-A translation table cursor.
pub type A32PageCursor<'a, H> = PageTable32Cursor<'a, A32PagingMetaData, A32PTE, H>;
