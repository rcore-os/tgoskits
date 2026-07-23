//! ARMv7-A specific page table structures.

use core::arch::asm;

use crate::{
    entry::arm::A32PTE,
    stage1::{PageTable32, PageTable32Cursor, PagingMetaData, TlbInvalidator, TlbScope},
};

/// ARMv7 local TLB invalidation; SMP callers must provide remote IPIs.
pub struct A32TlbInvalidator;

impl TlbInvalidator<ax_memory_addr::VirtAddr> for A32TlbInvalidator {
    const SCOPE: TlbScope = TlbScope::Local;

    #[inline]
    fn invalidate(vaddr: Option<ax_memory_addr::VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                asm!("mcr p15, 0, {0}, c8, c7, 1", in(reg) vaddr.as_usize());
            } else {
                let zero: usize = 0;
                asm!("mcr p15, 0, {0}, c8, c7, 0", in(reg) zero);
            }
            asm!("dsb");
            asm!("isb");
        }
    }
}

/// Metadata of ARMv7-A page tables.
pub struct A32PagingMetaData;

impl PagingMetaData for A32PagingMetaData {
    const LEVELS: usize = 2; // ARMv7-A uses 2-level page tables
    const PA_MAX_BITS: usize = 32;
    const VA_MAX_BITS: usize = 32;
    type VirtAddr = ax_memory_addr::VirtAddr;
    type Tlb = A32TlbInvalidator;

    fn vaddr_is_valid(_vaddr: usize) -> bool {
        // All 32-bit addresses are valid
        //     vaddr < 0xFFFF_FFFF
        true
    }
}

/// ARMv7-A Short-descriptor translation table.
pub type A32PageTable<H> = PageTable32<A32PagingMetaData, A32PTE, H>;
/// ARMv7-A translation table cursor.
pub type A32PageCursor<'a, H> = PageTable32Cursor<'a, A32PagingMetaData, A32PTE, H>;
