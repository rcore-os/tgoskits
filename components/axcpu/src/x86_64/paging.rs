//! x86_64 page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct X64PteFlags: u64 {
        const PRESENT = 1 << 0;
        const WRITABLE = 1 << 1;
        const USER = 1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const NO_CACHE = 1 << 4;
        const HUGE_PAGE = 1 << 7;
        const NO_EXECUTE = 1 << 63;
    }
}

/// x86_64 page-table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct X64Pte(u64);

impl X64Pte {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    fn flags(self) -> X64PteFlags {
        X64PteFlags::from_bits_truncate(self.0)
    }
}

impl PageTableEntry for X64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        if config.is_empty() {
            let huge = if is_huge {
                X64PteFlags::HUGE_PAGE.bits()
            } else {
                0
            };
            return Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | huge);
        }

        let mut flags = X64PteFlags::PRESENT;
        if config.contains(MappingFlags::WRITE) {
            flags |= X64PteFlags::WRITABLE;
        }
        if config.contains(MappingFlags::USER) {
            flags |= X64PteFlags::USER;
        }
        if config.intersects(MappingFlags::DEVICE | MappingFlags::UNCACHED) {
            flags |= X64PteFlags::NO_CACHE | X64PteFlags::WRITE_THROUGH;
        }
        if !config.contains(MappingFlags::EXECUTE) {
            flags |= X64PteFlags::NO_EXECUTE;
        }
        if is_huge {
            flags |= X64PteFlags::HUGE_PAGE;
        }
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(
            (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
                | (X64PteFlags::PRESENT | X64PteFlags::WRITABLE | X64PteFlags::USER).bits(),
        )
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(X64PteFlags::PRESENT) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::READ;
        config.set(MappingFlags::WRITE, flags.contains(X64PteFlags::WRITABLE));
        config.set(
            MappingFlags::EXECUTE,
            !flags.contains(X64PteFlags::NO_EXECUTE),
        );
        config.set(MappingFlags::USER, flags.contains(X64PteFlags::USER));
        config.set(
            MappingFlags::UNCACHED,
            flags.contains(X64PteFlags::NO_CACHE),
        );
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(X64PteFlags::PRESENT)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.flags().contains(X64PteFlags::HUGE_PAGE)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for X64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("X64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
