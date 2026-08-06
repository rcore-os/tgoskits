//! LoongArch64 page-table entry format.

use ax_memory_addr::PhysAddr;
use page_table_generic::{MemAttributes, PageTableEntry, PteConfig};

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

    fn leaf_flags(config: PteConfig) -> LaPteFlags {
        let mut flags = LaPteFlags::V | LaPteFlags::P;
        if !config.read {
            flags |= LaPteFlags::NR;
        }
        if config.writable {
            flags |= LaPteFlags::W | LaPteFlags::D;
        }
        if !config.executable {
            flags |= LaPteFlags::NX;
        }
        if config.lower {
            flags |= LaPteFlags::PLVL | LaPteFlags::PLVH;
        }
        match config.mem_attr {
            MemAttributes::Device => {}
            MemAttributes::Uncached => flags |= LaPteFlags::MATH,
            _ => flags |= LaPteFlags::MATL,
        }
        if config.huge {
            flags |= LaPteFlags::GH;
            if config.global {
                flags |= LaPteFlags::G;
            }
        } else if config.global {
            flags |= LaPteFlags::GH;
        }
        flags
    }

    fn is_table(self) -> bool {
        self.paddr().as_usize() != 0 && (self.0 & !Self::PHYS_ADDR_MASK) == 0
    }
}

impl PageTableEntry for La64Pte {
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let paddr = config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK;
        if config.is_dir && !config.huge {
            return Self(paddr);
        }
        Self(paddr | Self::leaf_flags(config).bits())
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let flags = self.flags();
        let table = is_dir && self.is_table();
        let valid = flags.contains(LaPteFlags::V) || table;
        let huge = is_dir && flags.contains(LaPteFlags::GH);
        let paddr = if huge {
            PhysAddr::from_usize(self.paddr().as_usize() & !(LaPteFlags::G.bits() as usize))
        } else {
            self.paddr()
        };
        PteConfig {
            paddr,
            valid,
            read: valid && !flags.contains(LaPteFlags::NR),
            writable: flags.contains(LaPteFlags::W),
            executable: valid && !flags.contains(LaPteFlags::NX),
            lower: flags.contains(LaPteFlags::PLVL | LaPteFlags::PLVH),
            dirty: flags.contains(LaPteFlags::D),
            global: if huge {
                flags.contains(LaPteFlags::G)
            } else {
                flags.contains(LaPteFlags::GH)
            },
            is_dir: table,
            huge,
            mem_attr: if !flags.contains(LaPteFlags::MATL) {
                if flags.contains(LaPteFlags::MATH) {
                    MemAttributes::Uncached
                } else {
                    MemAttributes::Device
                }
            } else {
                MemAttributes::Normal
            },
        }
    }

    fn valid(&self) -> bool {
        self.flags().contains(LaPteFlags::V) || self.is_table()
    }
}

impl core::fmt::Debug for La64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("La64Pte")
            .field("raw", &self.0)
            .field("config", &self.to_config(false))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use page_table_generic::{MappingFlags, PteConfig};

    use super::*;

    #[test]
    fn leaf_entries_follow_loongarch_flag_layout() {
        let pte = La64Pte::from_config(PteConfig::page(
            PhysAddr::from_usize(0x1234_5000),
            MappingFlags::READ | MappingFlags::WRITE,
            false,
        ));

        assert!(pte.flags().contains(LaPteFlags::V | LaPteFlags::P));
        assert!(pte.flags().contains(LaPteFlags::GH));
        assert!(pte.to_config(false).global);

        let missing_valid = La64Pte(pte.0 & !LaPteFlags::V.bits());
        assert!(!missing_valid.to_config(false).valid);
        assert!(!missing_valid.valid());

        let huge = La64Pte::from_config(PteConfig::page(
            PhysAddr::from_usize(0x4000_0000),
            MappingFlags::READ,
            true,
        ));
        assert!(huge.flags().contains(LaPteFlags::GH | LaPteFlags::G));
        assert!(huge.to_config(true).huge);
        assert!(huge.to_config(true).global);
        assert_eq!(
            huge.to_config(true).paddr,
            PhysAddr::from_usize(0x4000_0000)
        );
    }
}
