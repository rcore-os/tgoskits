// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use core::{arch::asm, fmt};

use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

bitflags::bitflags! {
    /// Memory attribute fields in VMSAv8-64 stage-2 descriptors.
    #[derive(Debug)]
    pub struct DescriptorAttr: u64 {
        const VALID =       1 << 0;
        const NON_BLOCK =   1 << 1;
        const ATTR =        0b1111 << 2;
        const S2AP_RO =     1 << 6;
        const S2AP_WO =     1 << 7;
        const INNER =       1 << 8;
        const SHAREABLE =   1 << 9;
        const AF =          1 << 10;
        const NG =          1 << 11;
        const CONTIGUOUS =  1 << 52;
        const XN =          1 << 54;
        const NS =          1 << 55;
        const PXN_TABLE =   1 << 59;
        const XN_TABLE =    1 << 60;
        const AP_NO_EL0_TABLE =   1 << 61;
        const AP_NO_WRITE_TABLE = 1 << 62;
        const NS_TABLE =    1 << 63;
    }
}

#[repr(u64)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum MemType {
    Device         = 0,
    Normal         = 1,
    NormalNonCache = 2,
}

impl DescriptorAttr {
    #[allow(clippy::unusual_byte_groupings)]
    const ATTR_INDEX_MASK: u64 = 0b1111_00;
    const PTE_S2_MEM_ATTR_NORMAL_INNER_WRITE_BACK_CACHEABLE: u64 = 0b11 << 2;
    const PTE_S2_MEM_ATTR_NORMAL_OUTER_WRITE_BACK_CACHEABLE: u64 = 0b11 << 4;
    const PTE_S2_MEM_ATTR_NORMAL_OUTER_WRITE_BACK_NOCACHEABLE: u64 = 0b1 << 4;
    const NORMAL_BIT: u64 = Self::PTE_S2_MEM_ATTR_NORMAL_INNER_WRITE_BACK_CACHEABLE
        | Self::PTE_S2_MEM_ATTR_NORMAL_OUTER_WRITE_BACK_CACHEABLE;

    const fn from_mem_type(mem_type: MemType) -> Self {
        let bits = match mem_type {
            MemType::Normal => Self::NORMAL_BIT | Self::SHAREABLE.bits(),
            MemType::NormalNonCache => {
                Self::PTE_S2_MEM_ATTR_NORMAL_INNER_WRITE_BACK_CACHEABLE
                    | Self::PTE_S2_MEM_ATTR_NORMAL_OUTER_WRITE_BACK_NOCACHEABLE
                    | Self::SHAREABLE.bits()
            }
            MemType::Device => Self::SHAREABLE.bits(),
        };
        Self::from_bits_retain(bits)
    }

    fn mem_type(&self) -> MemType {
        let idx = self.bits() & Self::ATTR_INDEX_MASK;
        match idx {
            Self::NORMAL_BIT => MemType::Normal,
            Self::PTE_S2_MEM_ATTR_NORMAL_OUTER_WRITE_BACK_NOCACHEABLE => MemType::NormalNonCache,
            0 => MemType::Device,
            _ => panic!("Invalid memory attribute index"),
        }
    }
}

impl From<DescriptorAttr> for MappingFlags {
    fn from(attr: DescriptorAttr) -> Self {
        let mut flags = Self::empty();
        if attr.contains(DescriptorAttr::VALID) {
            flags |= Self::READ;
        }
        if !attr.contains(DescriptorAttr::S2AP_WO) {
            flags |= Self::WRITE;
        }
        if !attr.contains(DescriptorAttr::XN) {
            flags |= Self::EXECUTE;
        }
        match attr.mem_type() {
            MemType::Device => flags |= Self::DEVICE,
            MemType::NormalNonCache => flags |= Self::UNCACHED,
            MemType::Normal => {}
        }
        flags
    }
}

impl From<MappingFlags> for DescriptorAttr {
    fn from(flags: MappingFlags) -> Self {
        let mut attr = if flags.contains(MappingFlags::DEVICE) {
            Self::from_mem_type(MemType::Device)
        } else if flags.contains(MappingFlags::UNCACHED) {
            Self::from_mem_type(MemType::NormalNonCache)
        } else {
            Self::from_mem_type(MemType::Normal)
        };
        if flags.contains(MappingFlags::READ) {
            attr |= Self::VALID | Self::S2AP_RO;
        }
        if flags.contains(MappingFlags::WRITE) {
            attr |= Self::S2AP_WO;
        }
        if !flags.contains(MappingFlags::EXECUTE) {
            attr |= Self::XN;
        }
        attr
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct A64PTEHV(u64);

impl A64PTEHV {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        DescriptorAttr::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for A64PTEHV {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let mut attr = if config.is_dir && !config.huge {
            DescriptorAttr::NON_BLOCK | DescriptorAttr::VALID
        } else {
            DescriptorAttr::from(config_to_flags(config)) | DescriptorAttr::AF
        };
        if !config.is_dir || !config.huge {
            attr |= DescriptorAttr::NON_BLOCK;
        }
        Self(attr.bits() | (config.paddr.raw() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let attr = DescriptorAttr::from_bits_truncate(self.0);
        let valid = self.valid();
        let non_block = attr.contains(DescriptorAttr::NON_BLOCK);
        let huge = is_dir && valid && !non_block;
        let mapping_flags = MappingFlags::from(attr);
        ptg::PteConfig {
            paddr: ptg::PhysAddr::new(self.paddr().as_usize()),
            valid,
            read: mapping_flags.contains(MappingFlags::READ),
            writable: mapping_flags.contains(MappingFlags::WRITE),
            executable: mapping_flags.contains(MappingFlags::EXECUTE),
            lower: mapping_flags.contains(MappingFlags::USER),
            is_dir: is_dir && valid && non_block,
            huge,
            mem_attr: if mapping_flags.contains(MappingFlags::DEVICE) {
                ptg::MemAttributes::Device
            } else if mapping_flags.contains(MappingFlags::UNCACHED) {
                ptg::MemAttributes::Uncached
            } else {
                ptg::MemAttributes::Normal
            },
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        DescriptorAttr::from_bits_truncate(self.0).contains(DescriptorAttr::VALID)
    }
}

impl fmt::Debug for A64PTEHV {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("A64PTE")
            .field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("attr", &DescriptorAttr::from_bits_truncate(self.0))
            .field("flags", &self.flags())
            .finish()
    }
}

#[derive(Copy, Clone)]
pub struct A64HVPagingMetaDataL3;

impl ptg::TableMeta for A64HVPagingMetaDataL3 {
    type P = A64PTEHV;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        // SAFETY: TLBI operations only invalidate stage-2 translations for the
        // current EL2 context; they do not dereference memory.
        unsafe {
            if let Some(vaddr) = vaddr {
                asm!("tlbi vae2is, {}; dsb sy; isb", in(reg) vaddr.raw())
            } else {
                asm!("tlbi alle2is; dsb sy; isb")
            }
        }
    }
}

#[derive(Copy, Clone)]
pub struct A64HVPagingMetaDataL4;

impl ptg::TableMeta for A64HVPagingMetaDataL4 {
    type P = A64PTEHV;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        A64HVPagingMetaDataL3::flush(vaddr);
    }
}

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<A64HVPagingMetaDataL3, A64HVPagingMetaDataL4, H, true>;

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
    match config.mem_attr {
        ptg::MemAttributes::Device => flags |= MappingFlags::DEVICE,
        ptg::MemAttributes::Uncached => flags |= MappingFlags::UNCACHED,
        _ => {}
    }
    flags
}
