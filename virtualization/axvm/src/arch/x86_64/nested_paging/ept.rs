//! Intel Extended Page Table entry encoding.

use core::{convert::TryFrom, fmt};

use ax_page_table::stage2 as ptg;
use axvm_types::{HostPhysAddr, MappingFlags};
use bit_field::BitField;

use super::runtime::{config_to_flags, flush_nested_page_table};

bitflags::bitflags! {
    /// EPT entry flags. (Intel SDM Vol. 3C, Section 28.3.2)
    struct EptFlags: u64 {
        const READ =                1 << 0;
        const WRITE =               1 << 1;
        const EXECUTE =             1 << 2;
        const MEM_TYPE_MASK =       0b111 << 3;
        const IGNORE_PAT =          1 << 6;
        const HUGE_PAGE =           1 << 7;
        const ACCESSED =            1 << 8;
        const DIRTY =               1 << 9;
        const EXECUTE_FOR_USER =    1 << 10;
    }
}

numeric_enum_macro::numeric_enum! {
    #[repr(u8)]
    #[derive(Debug, PartialEq, Clone, Copy)]
    /// EPT memory-type field values defined by the Intel SDM.
    enum EptMemoryType {
        Uncached = 0,
        WriteCombining = 1,
        WriteThrough = 4,
        WriteProtected = 5,
        WriteBack = 6,
    }
}

impl EptFlags {
    /// Update only the EPT memory-type bit field, preserving permission bits.
    fn set_memory_type(&mut self, memory_type: EptMemoryType) {
        let mut bits = self.bits();
        bits.set_bits(3..6, memory_type as u64);
        *self = Self::from_bits_truncate(bits)
    }

    /// Decode the memory-type field without accepting reserved encodings.
    fn memory_type(self) -> Result<EptMemoryType, u8> {
        EptMemoryType::try_from(self.bits().get_bits(3..6) as u8)
    }
}

impl From<MappingFlags> for EptFlags {
    fn from(flags: MappingFlags) -> Self {
        // A zero EPT entry is non-present; assigning a memory type without an
        // access permission would create an invalid leaf entry.
        if flags.is_empty() {
            return Self::empty();
        }
        let mut result = Self::empty();
        if flags.contains(MappingFlags::READ) {
            result |= Self::READ;
        }
        if flags.contains(MappingFlags::WRITE) {
            result |= Self::WRITE;
        }
        if flags.contains(MappingFlags::EXECUTE) {
            result |= Self::EXECUTE;
        }
        if flags.contains(MappingFlags::DEVICE) || flags.contains(MappingFlags::UNCACHED) {
            result.set_memory_type(EptMemoryType::Uncached);
        } else {
            result.set_memory_type(EptMemoryType::WriteBack);
        }
        result
    }
}

impl From<EptFlags> for MappingFlags {
    fn from(flags: EptFlags) -> Self {
        let mut result = MappingFlags::empty();
        if flags.contains(EptFlags::READ) {
            result |= MappingFlags::READ;
        }
        if flags.contains(EptFlags::WRITE) {
            result |= MappingFlags::WRITE;
        }
        if flags.contains(EptFlags::EXECUTE) {
            result |= MappingFlags::EXECUTE;
        }
        if matches!(flags.memory_type(), Ok(EptMemoryType::Uncached)) {
            result |= MappingFlags::DEVICE;
        }
        result
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
/// Raw EPT entry with Intel-specific permission and memory-type bits.
pub(super) struct EptEntry(u64);

impl EptEntry {
    // EPT entries store a 4 KiB-aligned host physical address. Masking keeps
    // flag and reserved bits out of the address passed to the generic walker.
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    fn paddr(self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(self) -> MappingFlags {
        EptFlags::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for EptEntry {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let flags = if config.is_dir && !config.huge {
            // Non-leaf EPT entries must permit the walk itself; leaf mapping
            // permissions are applied only at the final or huge-page entry.
            EptFlags::READ | EptFlags::WRITE | EptFlags::EXECUTE
        } else {
            let mut flags = EptFlags::from(config_to_flags(config));
            if config.huge {
                flags |= EptFlags::HUGE_PAGE;
            }
            flags
        };
        Self(flags.bits() | (config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = EptFlags::from_bits_truncate(self.0);
        let valid = self.valid();
        let huge = is_dir && flags.contains(EptFlags::HUGE_PAGE);
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
        self.0 & 0x7 != 0
    }
}

impl fmt::Debug for EptEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EptEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .field(
                "memory_type",
                &EptFlags::from_bits_truncate(self.0).memory_type(),
            )
            .finish()
    }
}

#[derive(Clone, Copy)]
/// EPT geometry and invalidation callback supplied to the generic walker.
pub(super) struct EptPageTableMetadata;

impl ptg::TableMeta for EptPageTableMetadata {
    type P = EptEntry;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        flush_nested_page_table(vaddr);
    }
}
