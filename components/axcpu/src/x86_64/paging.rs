//! x86_64 page-table entry format.

use ax_memory_addr::PhysAddr;
use page_table_generic::{MemAttributes, PageTableEntry, PteConfig};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct X64PteFlags: u64 {
        const PRESENT = 1 << 0;
        const WRITABLE = 1 << 1;
        const USER = 1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const NO_CACHE = 1 << 4;
        const DIRTY = 1 << 6;
        const HUGE_PAGE = 1 << 7;
        const GLOBAL = 1 << 8;
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
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        if config.is_dir && !config.huge {
            return Self(
                (config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
                    | (X64PteFlags::PRESENT | X64PteFlags::WRITABLE | X64PteFlags::USER).bits(),
            );
        }

        let mut flags = X64PteFlags::PRESENT;
        if config.writable {
            flags |= X64PteFlags::WRITABLE;
        }
        if config.lower {
            flags |= X64PteFlags::USER;
        }
        if matches!(
            config.mem_attr,
            MemAttributes::Device | MemAttributes::Uncached
        ) {
            flags |= X64PteFlags::NO_CACHE | X64PteFlags::WRITE_THROUGH;
        }
        if !config.executable {
            flags |= X64PteFlags::NO_EXECUTE;
        }
        if config.huge {
            flags |= X64PteFlags::HUGE_PAGE;
        }
        Self((config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let flags = self.flags();
        let valid = flags.contains(X64PteFlags::PRESENT);
        PteConfig {
            paddr: PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize),
            valid,
            read: valid,
            writable: flags.contains(X64PteFlags::WRITABLE),
            executable: valid && !flags.contains(X64PteFlags::NO_EXECUTE),
            lower: flags.contains(X64PteFlags::USER),
            dirty: flags.contains(X64PteFlags::DIRTY),
            global: flags.contains(X64PteFlags::GLOBAL),
            is_dir: is_dir && !flags.contains(X64PteFlags::HUGE_PAGE),
            huge: is_dir && flags.contains(X64PteFlags::HUGE_PAGE),
            mem_attr: if flags.contains(X64PteFlags::NO_CACHE) {
                MemAttributes::Uncached
            } else {
                MemAttributes::Normal
            },
        }
    }

    fn valid(&self) -> bool {
        self.flags().contains(X64PteFlags::PRESENT)
    }
}

impl core::fmt::Debug for X64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("X64Pte")
            .field("raw", &self.0)
            .field("config", &self.to_config(false))
            .finish()
    }
}
