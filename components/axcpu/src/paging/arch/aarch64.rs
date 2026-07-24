//! AArch64 specific page table structures.

use core::arch::asm;

use ax_memory_addr::VirtAddr;

use crate::paging::{
    PageTable64, PageTable64Cursor, PagingMetaData, TlbInvalidator, TlbScope,
    entry::aarch64::A64PTE,
};

/// AArch64 inner-shareable hardware TLB broadcast.
pub struct A64TlbInvalidator;

impl TlbInvalidator<VirtAddr> for A64TlbInvalidator {
    const SCOPE: TlbScope = TlbScope::HardwareBroadcast;

    #[inline]
    fn invalidate(vaddr: Option<VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                const VA_MASK: usize = (1 << 44) - 1;
                asm!("tlbi vaae1is, {}; dsb sy; isb", in(reg) ((vaddr.as_usize() >> 12) & VA_MASK))
            } else {
                asm!("tlbi vmalle1is; dsb sy; isb")
            }
        }
    }
}

/// Metadata of AArch64 page tables.
pub struct A64PagingMetaData;

impl PagingMetaData for A64PagingMetaData {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 48;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VirtAddr;
    type Tlb = A64TlbInvalidator;

    fn vaddr_is_valid(vaddr: usize) -> bool {
        let top_bits = vaddr >> Self::VA_MAX_BITS;
        top_bits == 0 || top_bits == 0xffff
    }
}

/// AArch64 VMSAv8-64 translation table.
pub type A64PageTable<H> = PageTable64<A64PagingMetaData, A64PTE, H>;
/// AArch64 VMSAv8-64 translation table cursor.
pub type A64PageTableCursor<'a, H> = PageTable64Cursor<'a, A64PagingMetaData, A64PTE, H>;
