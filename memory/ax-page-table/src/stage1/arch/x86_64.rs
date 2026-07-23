//! x86 specific page table structures.

use ax_memory_addr::VirtAddr;

use crate::{
    entry::x86_64::X64PTE,
    stage1::{PageTable64, PageTable64Cursor, PagingMetaData, TlbInvalidator, TlbScope},
};

/// x86 local TLB invalidation; SMP callers must provide remote IPIs.
pub struct X64TlbInvalidator;

impl TlbInvalidator<VirtAddr> for X64TlbInvalidator {
    const SCOPE: TlbScope = TlbScope::Local;

    #[inline]
    fn invalidate(vaddr: Option<VirtAddr>) {
        unsafe {
            if let Some(vaddr) = vaddr {
                x86::tlb::flush(vaddr.into());
            } else {
                x86::tlb::flush_all();
            }
        }
    }
}

/// Metadata of x86_64 page tables.
pub struct X64PagingMetaData<Tlb = X64TlbInvalidator>(core::marker::PhantomData<Tlb>);

impl<Tlb: TlbInvalidator<VirtAddr>> PagingMetaData for X64PagingMetaData<Tlb> {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 52;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VirtAddr;
    type Tlb = Tlb;
}

/// x86_64 page table.
pub type X64PageTable<H, Tlb = X64TlbInvalidator> = PageTable64<X64PagingMetaData<Tlb>, X64PTE, H>;
/// x86_64 page table cursor.
pub type X64PageTableCursor<'a, H, Tlb = X64TlbInvalidator> =
    PageTable64Cursor<'a, X64PagingMetaData<Tlb>, X64PTE, H>;
