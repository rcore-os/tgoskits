//! RISC-V page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct RvPteFlags: u64 {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
        // RSW bit 0 records the structural leaf shape while V is clear.
        const NON_PRESENT_HUGE = 1 << 8;
        #[cfg(feature = "xuantie-c9xx")]
        const SEC = 1 << 59;
        #[cfg(feature = "xuantie-c9xx")]
        const SH = 1 << 60;
        #[cfg(feature = "xuantie-c9xx")]
        const B = 1 << 61;
        #[cfg(feature = "xuantie-c9xx")]
        const C = 1 << 62;
        #[cfg(feature = "xuantie-c9xx")]
        const SO = 1 << 63;
    }
}

/// RISC-V Sv39/Sv48 page-table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Rv64Pte(u64);

impl Rv64Pte {
    const PHYS_ADDR_MASK: u64 = (1 << 54) - (1 << 10);

    fn flags(self) -> RvPteFlags {
        RvPteFlags::from_bits_truncate(self.0)
    }

    fn paddr(self) -> PhysAddr {
        PhysAddr::from_usize(((self.0 & Self::PHYS_ADDR_MASK) << 2) as usize)
    }

    fn leaf_flags(config: MappingFlags) -> RvPteFlags {
        let mut flags = RvPteFlags::A | RvPteFlags::D;
        if !config.is_empty() {
            flags |= RvPteFlags::V;
        }
        if config.intersects(MappingFlags::READ | MappingFlags::WRITE) {
            flags |= RvPteFlags::R;
        }
        if config.contains(MappingFlags::WRITE) {
            flags |= RvPteFlags::W;
        }
        if config.contains(MappingFlags::EXECUTE) {
            flags |= RvPteFlags::X;
        }
        if config.contains(MappingFlags::USER) {
            flags |= RvPteFlags::U;
        }
        #[cfg(feature = "xuantie-c9xx")]
        {
            if config.contains(MappingFlags::DEVICE) {
                flags |= RvPteFlags::SH | RvPteFlags::SO;
            } else if config.contains(MappingFlags::UNCACHED) {
                flags |= RvPteFlags::SH | RvPteFlags::B;
            } else {
                flags |= RvPteFlags::SH | RvPteFlags::B | RvPteFlags::C;
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
        let paddr = (paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK;
        let mut flags = Self::leaf_flags(config);
        if config.is_empty() && is_huge {
            flags |= RvPteFlags::NON_PRESENT_HUGE;
        }
        Self(paddr | flags.bits())
    }

    fn new_table(paddr: PhysAddr) -> Self {
        let paddr = (paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK;
        Self(paddr | RvPteFlags::V.bits())
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        Rv64Pte::paddr(*self)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(RvPteFlags::V) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, flags.contains(RvPteFlags::R));
        config.set(MappingFlags::WRITE, flags.contains(RvPteFlags::W));
        config.set(MappingFlags::EXECUTE, flags.contains(RvPteFlags::X));
        config.set(MappingFlags::USER, flags.contains(RvPteFlags::U));
        #[cfg(feature = "xuantie-c9xx")]
        {
            if flags.contains(RvPteFlags::SO) {
                config |= MappingFlags::DEVICE;
            } else if !flags.contains(RvPteFlags::C) {
                config |= MappingFlags::UNCACHED;
            }
        }
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(RvPteFlags::V)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir
            && self
                .flags()
                .intersects(RvPteFlags::R | RvPteFlags::X | RvPteFlags::NON_PRESENT_HUGE)
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
