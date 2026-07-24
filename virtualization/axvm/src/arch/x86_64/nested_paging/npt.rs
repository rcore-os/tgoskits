//! AMD Nested Page Table entry encoding.

use core::fmt;

use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

use super::runtime::{config_to_flags, flush_nested_page_table};

bitflags::bitflags! {
    /// AMD SVM nested page table entry flags.
    struct NptFlags: u64 {
        const PRESENT =       1 << 0;
        const WRITE =         1 << 1;
        const USER =          1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const NO_CACHE =      1 << 4;
        const ACCESSED =      1 << 5;
        const DIRTY =         1 << 6;
        const HUGE_PAGE =     1 << 7;
        const GLOBAL =        1 << 8;
        const NO_EXECUTE =    1 << 63;
    }
}

impl From<MappingFlags> for NptFlags {
    fn from(flags: MappingFlags) -> Self {
        // A non-present NPT entry is represented by zero. Permission flags are
        // meaningful only after PRESENT has been set.
        if flags.is_empty() {
            return Self::empty();
        }
        let mut result = Self::PRESENT;
        if flags.contains(MappingFlags::WRITE) {
            result |= Self::WRITE;
        }
        if flags.contains(MappingFlags::USER) {
            result |= Self::USER;
        }
        if !flags.contains(MappingFlags::EXECUTE) {
            result |= Self::NO_EXECUTE;
        }
        if flags.contains(MappingFlags::DEVICE) || flags.contains(MappingFlags::UNCACHED) {
            result |= Self::NO_CACHE | Self::WRITE_THROUGH;
        }
        result
    }
}

impl From<NptFlags> for MappingFlags {
    fn from(flags: NptFlags) -> Self {
        if !flags.contains(NptFlags::PRESENT) {
            return Self::empty();
        }
        let mut result = MappingFlags::READ;
        if flags.contains(NptFlags::WRITE) {
            result |= MappingFlags::WRITE;
        }
        if flags.contains(NptFlags::USER) {
            result |= MappingFlags::USER;
        }
        if !flags.contains(NptFlags::NO_EXECUTE) {
            result |= MappingFlags::EXECUTE;
        }
        if flags.contains(NptFlags::NO_CACHE) {
            result |= MappingFlags::DEVICE;
        }
        result
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
/// Raw NPT entry using the AMD long-mode page-table encoding.
pub(super) struct NptEntry(u64);

impl NptEntry {
    // NPT follows the long-mode page-table address layout: the host physical
    // page number occupies bits 12 through 51 and must exclude flag bits.
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    fn paddr(self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(self) -> MappingFlags {
        NptFlags::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for NptEntry {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let mut flags = if config.is_dir && !config.huge {
            // Intermediate NPT entries must remain present, writable, and
            // user-accessible so the guest walk reaches the leaf permission.
            NptFlags::PRESENT | NptFlags::WRITE | NptFlags::USER
        } else {
            NptFlags::from(config_to_flags(config))
        };
        if config.huge {
            flags |= NptFlags::HUGE_PAGE;
        }
        Self(flags.bits() | (config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = NptFlags::from_bits_truncate(self.0);
        let valid = self.valid();
        let huge = is_dir && flags.contains(NptFlags::HUGE_PAGE);
        let mapping_flags = MappingFlags::from(flags);
        ptg::PteConfig {
            paddr: ptg::PhysAddr::from_usize(self.paddr().as_usize()),
            valid,
            read: mapping_flags.contains(MappingFlags::READ),
            writable: mapping_flags.contains(MappingFlags::WRITE),
            executable: mapping_flags.contains(MappingFlags::EXECUTE),
            lower: mapping_flags.contains(MappingFlags::USER),
            is_dir: is_dir && valid && !huge,
            huge,
            mem_attr: if mapping_flags.contains(MappingFlags::DEVICE) {
                ptg::MemAttributes::Device
            } else {
                ptg::MemAttributes::Normal
            },
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        NptFlags::from_bits_truncate(self.0).contains(NptFlags::PRESENT)
    }
}

impl fmt::Debug for NptEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NptEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

#[derive(Clone, Copy)]
/// NPT geometry and invalidation callback supplied to the generic walker.
pub(super) struct NptPageTableMetadata;

impl ptg::TableMeta for NptPageTableMetadata {
    type P = NptEntry;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        flush_nested_page_table(vaddr);
    }
}
