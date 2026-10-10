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

/// Local-domain page-table metadata for the active target architecture.
///
/// A shared AArch64 stage-one table uses the runtime metadata in `ax-hal`,
/// which validates its platform domain before break-before-make.
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

    fn flush_batch(vaddrs: &[VirtAddr]) {
        #[cfg(target_arch = "riscv64")]
        if !vaddrs.is_empty() {
            // A batch may unlink a non-leaf PTE. SFENCE.VMA with a virtual
            // address only orders leaf PTE changes on RISC-V.
            Self::flush(None);
        }
        // Remote stage-1 shootdown and its completion receipt belong to the
        // runtime, not to architecture metadata's local flush operation.
        #[cfg(not(target_arch = "riscv64"))]
        for &vaddr in vaddrs {
            Self::flush(Some(vaddr));
        }
    }

    fn flush_leaf_batch(vaddrs: &[VirtAddr]) {
        #[cfg(target_arch = "riscv64")]
        for &vaddr in vaddrs {
            Self::flush(Some(vaddr));
        }
        #[cfg(not(target_arch = "riscv64"))]
        Self::flush_batch(vaddrs);
    }

    fn flush_before_make(vaddr: VirtAddr, page_size: usize) -> page_table_generic::PagingResult {
        if page_size > Self::PAGE_SIZE {
            Self::flush(None);
        } else {
            Self::flush_leaf_batch(core::slice::from_ref(&vaddr));
        }
        Ok(())
    }

    fn complete_replaced_leaf(vaddr: VirtAddr) {
        #[cfg(target_arch = "aarch64")]
        Self::publish_new_mapping(vaddr);
        #[cfg(not(target_arch = "aarch64"))]
        Self::flush_leaf_batch(core::slice::from_ref(&vaddr));
    }

    fn publish_new_mapping(vaddr: VirtAddr) {
        #[cfg(target_arch = "aarch64")]
        {
            let _ = vaddr;
            crate::asm::publish_new_mapping();
        }
        #[cfg(target_arch = "riscv64")]
        {
            let _ = vaddr;
            // An absent install can publish a complete new branch through a
            // formerly invalid non-leaf PTE, which requires rs1=x0.
            Self::flush(None);
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        Self::flush_batch(core::slice::from_ref(&vaddr));
    }
}
