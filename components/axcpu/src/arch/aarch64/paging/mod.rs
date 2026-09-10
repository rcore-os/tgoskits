//! AArch64 translation formats.

mod stage1;
mod stage2;

pub(crate) use stage1::{ADDRESS_BITS, LEVEL_BITS, MAX_BLOCK_LEVEL, PAGE_SIZE};
pub use stage1::{DescriptorFlags, El1Pte, El2Pte, Pte, Stage1Pte, Stage1Regime};
pub use stage2::Stage2Pte;

/// Four-level, 4-KiB non-VHE EL2 stage-one table geometry.
#[derive(Clone, Copy)]
pub struct El2PagingMeta;

impl crate::paging::TableMeta for El2PagingMeta {
    type P = El2Pte;
    const PAGE_SIZE: usize = PAGE_SIZE;
    const LEVEL_BITS: &'static [usize] = LEVEL_BITS;
    const MAX_BLOCK_LEVEL: usize = MAX_BLOCK_LEVEL;

    fn canonicalize_vaddr(address: crate::VirtAddr) -> crate::VirtAddr {
        crate::VirtAddr::from_usize(address.as_usize() & ((1usize << ADDRESS_BITS) - 1))
    }

    fn flush(address: Option<crate::VirtAddr>) {
        super::mmu::El2::flush_tlb(address);
    }
}
