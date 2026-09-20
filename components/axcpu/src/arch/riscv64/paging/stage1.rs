//! RISC-V page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    /// Hardware descriptor flags for Sv39/Sv48 and optional T-Head MAE.
    pub struct DescriptorFlags: u64 {
        /// Valid translation.
        const V = 1 << 0;
        /// Readable leaf.
        const R = 1 << 1;
        /// Writable leaf.
        const W = 1 << 2;
        /// Executable leaf.
        const X = 1 << 3;
        /// User-accessible leaf.
        const U = 1 << 4;
        /// Global translation.
        const G = 1 << 5;
        /// Accessed.
        const A = 1 << 6;
        /// Dirty.
        const D = 1 << 7;
        // RSW bit 0 records the structural leaf shape while V is clear.
        /// Software marker retaining the shape of an invalid block.
        const NON_PRESENT_HUGE = 1 << 8;
        #[cfg(feature = "riscv-thead-mae")]
        /// T-Head secure memory.
        const SEC = 1 << 59;
        #[cfg(feature = "riscv-thead-mae")]
        /// T-Head shared memory.
        const SH = 1 << 60;
        #[cfg(feature = "riscv-thead-mae")]
        /// T-Head bufferable memory.
        const B = 1 << 61;
        #[cfg(feature = "riscv-thead-mae")]
        /// T-Head cacheable memory.
        const C = 1 << 62;
        #[cfg(feature = "riscv-thead-mae")]
        /// T-Head strongly ordered memory.
        const SO = 1 << 63;
    }
}

/// RISC-V Sv39/Sv48 page-table entry.
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct Rv64Pte(u64);

/// Native stage-one page-table descriptor.
pub type Pte = Rv64Pte;

impl Rv64Pte {
    const PHYS_ADDR_MASK: u64 = (1 << 54) - (1 << 10);

    /// Returns the descriptor flags, excluding the physical page number.
    pub fn flags(self) -> DescriptorFlags {
        DescriptorFlags::from_bits_truncate(self.0)
    }

    fn physical_page_base(self) -> PhysAddr {
        PhysAddr::from_usize(((self.0 & Self::PHYS_ADDR_MASK) << 2) as usize)
    }

    /// Encodes a physical page base and flags without selecting mapping policy.
    /// Low address bits are discarded; callers installing the descriptor must
    /// enforce the selected paging mode's alignment and reserved-bit rules.
    pub const fn from_parts(paddr: PhysAddr, flags: DescriptorFlags) -> Self {
        Self(((paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    fn leaf_flags(config: MappingFlags) -> DescriptorFlags {
        let mut flags = DescriptorFlags::A | DescriptorFlags::D;
        if !config.is_empty() {
            flags |= DescriptorFlags::V;
        }
        if config.intersects(MappingFlags::READ | MappingFlags::WRITE) {
            flags |= DescriptorFlags::R;
        }
        if config.contains(MappingFlags::WRITE) {
            flags |= DescriptorFlags::W;
        }
        if config.contains(MappingFlags::EXECUTE) {
            flags |= DescriptorFlags::X;
        }
        if config.contains(MappingFlags::USER) {
            flags |= DescriptorFlags::U;
        }
        #[cfg(feature = "riscv-thead-mae")]
        {
            if config.contains(MappingFlags::DEVICE) {
                flags |= DescriptorFlags::SH | DescriptorFlags::SO;
            } else if config.contains(MappingFlags::UNCACHED) {
                flags |= DescriptorFlags::SH | DescriptorFlags::B;
            } else {
                flags |= DescriptorFlags::SH | DescriptorFlags::B | DescriptorFlags::C;
            }
        }
        flags
    }
}

impl PageTableEntry for Rv64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        let mut flags = Self::leaf_flags(config);
        if config.is_empty() && is_huge {
            flags |= DescriptorFlags::NON_PRESENT_HUGE;
        }
        Self::from_parts(paddr, flags)
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self::from_parts(paddr, DescriptorFlags::V)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        self.physical_page_base()
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(DescriptorFlags::V) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, flags.contains(DescriptorFlags::R));
        config.set(MappingFlags::WRITE, flags.contains(DescriptorFlags::W));
        config.set(MappingFlags::EXECUTE, flags.contains(DescriptorFlags::X));
        config.set(MappingFlags::USER, flags.contains(DescriptorFlags::U));
        #[cfg(feature = "riscv-thead-mae")]
        {
            if flags.contains(DescriptorFlags::SO) {
                config |= MappingFlags::DEVICE;
            } else if !flags.contains(DescriptorFlags::C) {
                config |= MappingFlags::UNCACHED;
            }
        }
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(DescriptorFlags::V)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir
            && self.flags().intersects(
                DescriptorFlags::R | DescriptorFlags::X | DescriptorFlags::NON_PRESENT_HUGE,
            )
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for Rv64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rv64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
