//! Page-table metadata for the active architecture.

use ax_memory_addr::VirtAddr;
use page_table_generic::TableMeta;

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

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        use crate::x86_64::paging::{
            LEVEL_BITS as ARCH_LEVEL_BITS, MAX_BLOCK_LEVEL as ARCH_MAX_BLOCK_LEVEL,
            PAGE_SIZE as ARCH_PAGE_SIZE, X64Pte as ArchPte,
        };
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        use crate::riscv::paging::{
            LEVEL_BITS as ARCH_LEVEL_BITS, MAX_BLOCK_LEVEL as ARCH_MAX_BLOCK_LEVEL,
            PAGE_SIZE as ARCH_PAGE_SIZE, Rv64Pte as ArchPte,
        };
    } else if #[cfg(target_arch = "aarch64")] {
        use crate::aarch64::paging::{
            A64Pte as ArchPte, LEVEL_BITS as ARCH_LEVEL_BITS,
            MAX_BLOCK_LEVEL as ARCH_MAX_BLOCK_LEVEL, PAGE_SIZE as ARCH_PAGE_SIZE,
        };
    } else if #[cfg(target_arch = "loongarch64")] {
        use crate::loongarch64::paging::{
            LEVEL_BITS as ARCH_LEVEL_BITS, La64Pte as ArchPte,
            MAX_BLOCK_LEVEL as ARCH_MAX_BLOCK_LEVEL, PAGE_SIZE as ARCH_PAGE_SIZE,
        };
    }
}

/// Page-table metadata for the active target architecture.
#[derive(Clone, Copy)]
pub struct ArchPagingMeta;

impl TableMeta for ArchPagingMeta {
    type P = ArchPte;

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
