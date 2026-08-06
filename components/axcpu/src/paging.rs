//! Page-table metadata for the active architecture.

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr};
use page_table_generic::TableMeta;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        use crate::x86_64::paging::X64Pte as ArchPte;
        const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        use crate::riscv::paging::Rv64Pte as ArchPte;
        const LEVEL_BITS: &[usize] = &[9, 9, 9];
    } else if #[cfg(target_arch = "aarch64")] {
        use crate::aarch64::paging::A64Pte as ArchPte;
        const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    } else if #[cfg(target_arch = "loongarch64")] {
        use crate::loongarch64::paging::La64Pte as ArchPte;
        const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    }
}

/// Page-table metadata for the active target architecture.
#[derive(Clone, Copy)]
pub struct ArchPagingMeta;

impl TableMeta for ArchPagingMeta {
    type P = ArchPte;

    const PAGE_SIZE: usize = PAGE_SIZE_4K;
    const LEVEL_BITS: &'static [usize] = LEVEL_BITS;
    const MAX_BLOCK_LEVEL: usize = 3;

    fn canonicalize_vaddr(vaddr: VirtAddr) -> VirtAddr {
        let address_bits =
            PAGE_SIZE_4K.trailing_zeros() as usize + LEVEL_BITS.iter().copied().sum::<usize>();
        let mask = (1usize << address_bits) - 1;
        let address = vaddr.as_usize() & mask;
        if address & (1usize << (address_bits - 1)) == 0 {
            VirtAddr::from_usize(address)
        } else {
            VirtAddr::from_usize(address | !mask)
        }
    }

    fn flush(vaddr: Option<VirtAddr>) {
        crate::asm::flush_tlb(vaddr);
    }
}
