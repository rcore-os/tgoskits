//! RISC-V page-table entry format.

use ax_memory_addr::PhysAddr;
use page_table_generic::{MemAttributes, PageTableEntry, PteConfig};

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

    fn leaf_flags(config: PteConfig) -> RvPteFlags {
        let mut flags = RvPteFlags::V | RvPteFlags::A | RvPteFlags::D;
        if config.read || config.writable {
            flags |= RvPteFlags::R;
        }
        if config.writable {
            flags |= RvPteFlags::W;
        }
        if config.executable {
            flags |= RvPteFlags::X;
        }
        if config.lower {
            flags |= RvPteFlags::U;
        }
        #[cfg(feature = "xuantie-c9xx")]
        {
            if matches!(config.mem_attr, MemAttributes::Device) {
                flags |= RvPteFlags::SH | RvPteFlags::SO;
            } else if matches!(config.mem_attr, MemAttributes::Uncached) {
                flags |= RvPteFlags::SH | RvPteFlags::B;
            } else {
                flags |= RvPteFlags::SH | RvPteFlags::B | RvPteFlags::C;
            }
        }
        flags
    }
}

impl PageTableEntry for Rv64Pte {
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let paddr = (config.paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK;
        let flags = if config.is_dir && !config.huge {
            RvPteFlags::V
        } else {
            Self::leaf_flags(config)
        };
        Self(paddr | flags.bits())
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let flags = self.flags();
        let valid = flags.contains(RvPteFlags::V);
        let huge = is_dir && flags.intersects(RvPteFlags::R | RvPteFlags::X);
        PteConfig {
            paddr: self.paddr(),
            valid,
            read: flags.contains(RvPteFlags::R),
            writable: flags.contains(RvPteFlags::W),
            executable: flags.contains(RvPteFlags::X),
            lower: flags.contains(RvPteFlags::U),
            dirty: flags.contains(RvPteFlags::D),
            global: flags.contains(RvPteFlags::G),
            is_dir: is_dir && valid && !huge,
            huge,
            mem_attr: MemAttributes::Normal,
        }
    }

    fn valid(&self) -> bool {
        self.flags().contains(RvPteFlags::V)
    }
}

impl core::fmt::Debug for Rv64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rv64Pte")
            .field("raw", &self.0)
            .field("config", &self.to_config(false))
            .finish()
    }
}
