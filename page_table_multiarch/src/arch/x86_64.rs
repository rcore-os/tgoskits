//! x86 specific page table structures.

use memory_addr::VirtAddr;
use page_table_entry::x86_64::X64PTE;

use crate::{PageTable64, PagingMetaData};

/// metadata of x86_64 page tables.
pub struct X64PagingMetaData;

impl PagingMetaData for X64PagingMetaData {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 52;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VirtAddr;

    #[inline]
    fn flush_tlb(vaddr: Option<VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                x86::tlb::flush(vaddr.into());
            } else {
                x86::tlb::flush_all();
            }
        }
    }
}

/// x86_64 page table.
pub type X64PageTable<H> = PageTable64<X64PagingMetaData, X64PTE, H>;
