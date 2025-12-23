//! AArch64 specific page table structures.

use core::arch::asm;

use memory_addr::VirtAddr;
use page_table_entry::aarch64::A64PTE;

use crate::{PageTable64, PagingMetaData};

/// Metadata of AArch64 page tables.
pub struct A64PagingMetaData;

impl PagingMetaData for A64PagingMetaData {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 48;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VirtAddr;

    fn vaddr_is_valid(vaddr: usize) -> bool {
        let top_bits = vaddr >> Self::VA_MAX_BITS;
        top_bits == 0 || top_bits == 0xffff
    }

    #[inline]
    fn flush_tlb(vaddr: Option<VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                // TLB Invalidate by VA, All ASID, EL1, Inner Shareable
                const VA_MASK: usize = (1 << 44) - 1; // VA[55:12] => bits[43:0]
                asm!("tlbi vaae1is, {}; dsb sy; isb", in(reg) ((vaddr.as_usize() >> 12) & VA_MASK))
            } else {
                // TLB Invalidate by VMID, All at stage 1, EL1
                asm!("tlbi vmalle1; dsb sy; isb")
            }
        }
    }
}

/// AArch64 VMSAv8-64 translation table.
pub type A64PageTable<H> = PageTable64<A64PagingMetaData, A64PTE, H>;
