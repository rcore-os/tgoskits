//! LoongArch64 page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

/// PWCL fields matching the runtime page-table geometry.
pub(super) const PWCL_VALUE: u32 = 12 | (9 << 5) | (21 << 10) | (9 << 15) | (30 << 20) | (9 << 25);
/// PWCH fields matching the runtime page-table geometry.
pub(super) const PWCH_VALUE: u32 = 39 | (9 << 6);

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct LaPteFlags: u64 {
        const V = 1 << 0;
        const D = 1 << 1;
        const PLVL = 1 << 2;
        const PLVH = 1 << 3;
        const MATL = 1 << 4;
        const MATH = 1 << 5;
        const GH = 1 << 6;
        const P = 1 << 7;
        const W = 1 << 8;
        const G = 1 << 12;
        const NR = 1 << 61;
        const NX = 1 << 62;
    }
}

/// LoongArch64 page-table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct La64Pte(u64);

impl La64Pte {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    fn flags(self) -> LaPteFlags {
        LaPteFlags::from_bits_truncate(self.0)
    }

    fn paddr(self) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn leaf_flags(config: MappingFlags, is_huge: bool) -> LaPteFlags {
        if config.is_empty() {
            return if is_huge {
                LaPteFlags::GH
            } else {
                // Keep a non-present leaf distinct from an address-only directory entry.
                LaPteFlags::P
            };
        }
        let mut flags = LaPteFlags::V | LaPteFlags::P;
        if !config.contains(MappingFlags::READ) {
            flags |= LaPteFlags::NR;
        }
        if config.contains(MappingFlags::WRITE) {
            flags |= LaPteFlags::W | LaPteFlags::D;
        }
        if !config.contains(MappingFlags::EXECUTE) {
            flags |= LaPteFlags::NX;
        }
        if config.contains(MappingFlags::USER) {
            flags |= LaPteFlags::PLVL | LaPteFlags::PLVH;
        }
        if config.contains(MappingFlags::UNCACHED) {
            flags |= LaPteFlags::MATH;
        } else if !config.contains(MappingFlags::DEVICE) {
            flags |= LaPteFlags::MATL;
        }
        let global = !config.contains(MappingFlags::USER);
        if is_huge {
            flags |= LaPteFlags::GH;
            if global {
                flags |= LaPteFlags::G;
            }
        } else if global {
            flags |= LaPteFlags::GH;
        }
        flags
    }

    fn is_table(self) -> bool {
        self.paddr().as_usize() != 0 && (self.0 & !Self::PHYS_ADDR_MASK) == 0
    }
}

impl PageTableEntry for La64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        let paddr = paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK;
        Self(paddr | Self::leaf_flags(config, is_huge).bits())
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
    }

    fn paddr(&self, is_dir: bool) -> PhysAddr {
        let flags = self.flags();
        let huge = is_dir && flags.contains(LaPteFlags::GH);
        if huge {
            PhysAddr::from_usize(
                La64Pte::paddr(*self).as_usize() & !(LaPteFlags::G.bits() as usize),
            )
        } else {
            La64Pte::paddr(*self)
        }
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(LaPteFlags::V) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, !flags.contains(LaPteFlags::NR));
        config.set(MappingFlags::WRITE, flags.contains(LaPteFlags::W));
        config.set(MappingFlags::EXECUTE, !flags.contains(LaPteFlags::NX));
        config.set(
            MappingFlags::USER,
            flags.contains(LaPteFlags::PLVL | LaPteFlags::PLVH),
        );
        if !flags.contains(LaPteFlags::MATL) {
            config |= if flags.contains(LaPteFlags::MATH) {
                MappingFlags::UNCACHED
            } else {
                MappingFlags::DEVICE
            };
        }
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(LaPteFlags::V) || self.is_table()
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.flags().contains(LaPteFlags::GH)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for La64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("La64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_entries_follow_loongarch_flag_layout() {
        let pte = La64Pte::new_page(
            PhysAddr::from_usize(0x1234_5000),
            MappingFlags::READ | MappingFlags::WRITE,
            false,
        );

        assert!(pte.flags().contains(LaPteFlags::V | LaPteFlags::P));
        assert!(pte.flags().contains(LaPteFlags::GH));
        assert!(!pte.config(false).contains(MappingFlags::USER));

        let missing_valid = La64Pte(pte.0 & !LaPteFlags::V.bits());
        assert!(missing_valid.config(false).is_empty());
        assert!(!missing_valid.present());

        let non_present = La64Pte::new_page(
            PhysAddr::from_usize(0x2345_6000),
            MappingFlags::empty(),
            false,
        );
        assert!(non_present.config(false).is_empty());
        assert!(!non_present.unused());
        assert_eq!(
            PageTableEntry::paddr(&non_present, false),
            PhysAddr::from_usize(0x2345_6000)
        );

        let huge = La64Pte::new_page(PhysAddr::from_usize(0x4000_0000), MappingFlags::READ, true);
        assert!(huge.flags().contains(LaPteFlags::GH | LaPteFlags::G));
        assert!(huge.huge(true));
        assert!(!huge.config(true).contains(MappingFlags::USER));
        assert_eq!(
            PageTableEntry::paddr(&huge, true),
            PhysAddr::from_usize(0x4000_0000)
        );
    }
}
