// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use core::{convert::TryFrom, fmt};

use axvm_types::{HostPhysAddr, MappingFlags};
use bit_field::BitField;
use page_table_generic as ptg;

bitflags::bitflags! {
    /// EPT entry flags. (SDM Vol. 3C, Section 28.3.2)
    struct EPTFlags: u64 {
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
    enum EPTMemType {
        Uncached = 0,
        WriteCombining = 1,
        WriteThrough = 4,
        WriteProtected = 5,
        WriteBack = 6,
    }
}

impl EPTFlags {
    fn set_mem_type(&mut self, mem_type: EPTMemType) {
        let mut bits = self.bits();
        bits.set_bits(3..6, mem_type as u64);
        *self = Self::from_bits_truncate(bits)
    }

    fn mem_type(&self) -> Result<EPTMemType, u8> {
        EPTMemType::try_from(self.bits().get_bits(3..6) as u8)
    }
}

impl From<MappingFlags> for EPTFlags {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::empty();
        if flags.contains(MappingFlags::READ) {
            ret |= Self::READ;
        }
        if flags.contains(MappingFlags::WRITE) {
            ret |= Self::WRITE;
        }
        if flags.contains(MappingFlags::EXECUTE) {
            ret |= Self::EXECUTE;
        }
        if flags.contains(MappingFlags::DEVICE) || flags.contains(MappingFlags::UNCACHED) {
            ret.set_mem_type(EPTMemType::Uncached);
        } else {
            ret.set_mem_type(EPTMemType::WriteBack);
        }
        ret
    }
}

impl From<EPTFlags> for MappingFlags {
    fn from(flags: EPTFlags) -> Self {
        let mut ret = MappingFlags::empty();
        if flags.contains(EPTFlags::READ) {
            ret |= MappingFlags::READ;
        }
        if flags.contains(EPTFlags::WRITE) {
            ret |= MappingFlags::WRITE;
        }
        if flags.contains(EPTFlags::EXECUTE) {
            ret |= MappingFlags::EXECUTE;
        }
        if matches!(flags.mem_type(), Ok(EPTMemType::Uncached)) {
            ret |= MappingFlags::DEVICE;
        }
        ret
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg_attr(feature = "svm", allow(dead_code))]
pub struct EPTEntry(u64);

#[cfg_attr(feature = "svm", allow(dead_code))]
impl EPTEntry {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        EPTFlags::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for EPTEntry {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let flags = if config.is_dir && !config.huge {
            EPTFlags::READ | EPTFlags::WRITE | EPTFlags::EXECUTE
        } else {
            let mut flags = EPTFlags::from(config_to_flags(config));
            if config.huge {
                flags |= EPTFlags::HUGE_PAGE;
            }
            flags
        };
        Self(flags.bits() | (config.paddr.raw() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = EPTFlags::from_bits_truncate(self.0);
        let valid = self.valid();
        let huge = is_dir && flags.contains(EPTFlags::HUGE_PAGE);
        let mapping_flags = MappingFlags::from(flags);
        ptg::PteConfig {
            paddr: ptg::PhysAddr::new(self.paddr().as_usize()),
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

impl fmt::Debug for EPTEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EPTEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .field("mem_type", &EPTFlags::from_bits_truncate(self.0).mem_type())
            .finish()
    }
}

bitflags::bitflags! {
    /// AMD SVM nested page table entry flags.
    struct NPTFlags: u64 {
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

impl From<MappingFlags> for NPTFlags {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::PRESENT;
        if flags.contains(MappingFlags::WRITE) {
            ret |= Self::WRITE;
        }
        if flags.contains(MappingFlags::USER) {
            ret |= Self::USER;
        }
        if !flags.contains(MappingFlags::EXECUTE) {
            ret |= Self::NO_EXECUTE;
        }
        if flags.contains(MappingFlags::DEVICE) || flags.contains(MappingFlags::UNCACHED) {
            ret |= Self::NO_CACHE | Self::WRITE_THROUGH;
        }
        ret
    }
}

impl From<NPTFlags> for MappingFlags {
    fn from(flags: NPTFlags) -> Self {
        if !flags.contains(NPTFlags::PRESENT) {
            return Self::empty();
        }
        let mut ret = MappingFlags::READ;
        if flags.contains(NPTFlags::WRITE) {
            ret |= MappingFlags::WRITE;
        }
        if flags.contains(NPTFlags::USER) {
            ret |= MappingFlags::USER;
        }
        if !flags.contains(NPTFlags::NO_EXECUTE) {
            ret |= MappingFlags::EXECUTE;
        }
        if flags.contains(NPTFlags::NO_CACHE) {
            ret |= MappingFlags::DEVICE;
        }
        ret
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg_attr(not(feature = "svm"), allow(dead_code))]
pub struct NPTEntry(u64);

#[cfg_attr(not(feature = "svm"), allow(dead_code))]
impl NPTEntry {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        NPTFlags::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for NPTEntry {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let mut flags = if config.is_dir && !config.huge {
            NPTFlags::PRESENT | NPTFlags::WRITE | NPTFlags::USER
        } else {
            NPTFlags::from(config_to_flags(config))
        };
        if config.huge {
            flags |= NPTFlags::HUGE_PAGE;
        }
        Self(flags.bits() | (config.paddr.raw() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = NPTFlags::from_bits_truncate(self.0);
        let valid = self.valid();
        let huge = is_dir && flags.contains(NPTFlags::HUGE_PAGE);
        let mapping_flags = MappingFlags::from(flags);
        ptg::PteConfig {
            paddr: ptg::PhysAddr::new(self.paddr().as_usize()),
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
        NPTFlags::from_bits_truncate(self.0).contains(NPTFlags::PRESENT)
    }
}

impl fmt::Debug for NPTEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NPTEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct ExtendedPageTableMetadata;

impl ptg::TableMeta for ExtendedPageTableMetadata {
    type P = ExtendedPageTableEntry;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    #[allow(unused_variables)]
    fn flush(vaddr: Option<ptg::VirtAddr>) {
        #[cfg(target_os = "none")]
        {
            if let Some(vaddr) = vaddr {
                unsafe { x86::tlb::flush(vaddr.raw()) }
            } else {
                unsafe { x86::tlb::flush_all() }
            }
        }
    }
}

#[cfg(not(feature = "svm"))]
pub type ExtendedPageTableEntry = EPTEntry;

#[cfg(feature = "svm")]
pub type ExtendedPageTableEntry = NPTEntry;

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<ExtendedPageTableMetadata, ExtendedPageTableMetadata, H, false>;

fn config_to_flags(config: ptg::PteConfig) -> MappingFlags {
    let mut flags = MappingFlags::empty();
    if config.read {
        flags |= MappingFlags::READ;
    }
    if config.writable {
        flags |= MappingFlags::WRITE;
    }
    if config.executable {
        flags |= MappingFlags::EXECUTE;
    }
    if config.lower {
        flags |= MappingFlags::USER;
    }
    match config.mem_attr {
        ptg::MemAttributes::Device => flags |= MappingFlags::DEVICE,
        ptg::MemAttributes::Uncached => flags |= MappingFlags::UNCACHED,
        _ => {}
    }
    flags
}
