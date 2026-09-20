//! Page-table metadata for the active architecture.

pub use page_table_generic::{PageTableEntry, TableMeta};
#[cfg(target_arch = "x86_64")]
pub use x86_64::structures::paging::{PageOffset, PageTableIndex, page_table::PageTableLevel};

use crate::VirtAddr;

bitflags::bitflags! {
    /// Runtime stage-1 mapping permissions and memory attributes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MappingFlags: usize {
        /// The memory is readable.
        const READ = 1 << 0;
        /// The memory is writable.
        const WRITE = 1 << 1;
        /// The memory is executable.
        const EXECUTE = 1 << 2;
        /// The memory is accessible from a lower-privileged context.
        const USER = 1 << 3;
        /// The memory is device memory.
        const DEVICE = 1 << 4;
        /// The memory is uncached.
        const UNCACHED = 1 << 5;
    }
}

impl From<crate::trap::PageFaultFlags> for MappingFlags {
    fn from(fault: crate::trap::PageFaultFlags) -> Self {
        let mut flags = Self::empty();
        flags.set(
            Self::READ,
            fault.contains(crate::trap::PageFaultFlags::READ),
        );
        flags.set(
            Self::WRITE,
            fault.contains(crate::trap::PageFaultFlags::WRITE),
        );
        flags.set(
            Self::EXECUTE,
            fault.contains(crate::trap::PageFaultFlags::EXECUTE),
        );
        flags.set(
            Self::USER,
            fault.contains(crate::trap::PageFaultFlags::USER),
        );
        flags
    }
}

pub use crate::arch::current::paging::{DescriptorFlags, Pte};
#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::paging::{
    El1Pte, El2PagingMeta, El2Pte, Stage1Pte, Stage1Regime, Stage2Pte,
};
#[cfg(target_arch = "x86_64")]
pub use crate::arch::current::paging::{EptEntry, EptFlags, EptMemoryType, NptEntry};
use crate::arch::current::paging::{
    LEVEL_BITS as ARCH_LEVEL_BITS, MAX_BLOCK_LEVEL as ARCH_MAX_BLOCK_LEVEL,
    PAGE_SIZE as ARCH_PAGE_SIZE,
};

/// Page-table metadata for the active target architecture.
#[derive(Clone, Copy)]
pub struct ArchPagingMeta;

impl TableMeta for ArchPagingMeta {
    type P = Pte;

    const PAGE_SIZE: usize = ARCH_PAGE_SIZE;
    const LEVEL_BITS: &'static [usize] = ARCH_LEVEL_BITS;
    const MAX_BLOCK_LEVEL: usize = ARCH_MAX_BLOCK_LEVEL;

    fn canonicalize_vaddr(vaddr: VirtAddr) -> VirtAddr {
        let address_bits = ARCH_PAGE_SIZE.trailing_zeros() as usize
            + ARCH_LEVEL_BITS.iter().copied().sum::<usize>();
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
