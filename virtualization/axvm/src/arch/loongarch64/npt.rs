// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use core::{arch::asm, fmt};

use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

bitflags::bitflags! {
    #[derive(Debug)]
    struct PTEFlags: u64 {
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
        const RPLV = 1 << 63;
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(flags: PTEFlags) -> Self {
        if !flags.contains(PTEFlags::V) {
            return Self::empty();
        }

        let mut ret = Self::empty();
        if !flags.contains(PTEFlags::NR) {
            ret |= Self::READ;
        }
        if flags.contains(PTEFlags::W) {
            ret |= Self::WRITE;
        }
        if !flags.contains(PTEFlags::NX) {
            ret |= Self::EXECUTE;
        }
        if flags.contains(PTEFlags::PLVL | PTEFlags::PLVH) {
            ret |= Self::USER;
        }
        if !flags.contains(PTEFlags::MATL) {
            if flags.contains(PTEFlags::MATH) {
                ret |= Self::UNCACHED;
            } else {
                ret |= Self::DEVICE;
            }
        }
        ret
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            return Self::empty();
        }

        let mut ret = Self::V | Self::P;
        if !flags.contains(MappingFlags::READ) {
            ret |= Self::NR;
        }
        if flags.contains(MappingFlags::WRITE) {
            ret |= Self::W | Self::D;
        }
        if !flags.contains(MappingFlags::EXECUTE) {
            ret |= Self::NX;
        }
        if flags.contains(MappingFlags::USER) {
            ret |= Self::PLVH | Self::PLVL;
        }
        if !flags.contains(MappingFlags::DEVICE) {
            if flags.contains(MappingFlags::UNCACHED) {
                ret |= Self::MATH;
            } else {
                ret |= Self::MATL;
            }
        }
        ret
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct LoongArchPTE(u64);

impl LoongArchPTE {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        PTEFlags::from_bits_truncate(self.0).into()
    }
}

impl ptg::PageTableEntry for LoongArchPTE {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let mut flags = if config.is_dir && !config.huge {
            PTEFlags::V | PTEFlags::P | PTEFlags::MATL
        } else {
            PTEFlags::from(config_to_flags(config))
        };
        if config.huge {
            flags |= PTEFlags::GH;
        }
        Self(flags.bits() | (config.paddr.raw() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = PTEFlags::from_bits_truncate(self.0);
        let valid = self.valid();
        let huge = is_dir && flags.contains(PTEFlags::GH);
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
            } else if mapping_flags.contains(MappingFlags::UNCACHED) {
                ptg::MemAttributes::Uncached
            } else {
                ptg::MemAttributes::Normal
            },
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0).contains(PTEFlags::V)
    }
}

impl fmt::Debug for LoongArchPTE {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoongArchPTE")
            .field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

#[derive(Copy, Clone)]
pub struct LoongArchPagingMetaDataL3;

impl ptg::TableMeta for LoongArchPagingMetaDataL3 {
    type P = LoongArchPTE;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        // SAFETY: `invtlb 0x0` invalidates translations globally and does not
        // dereference memory. It is conservative but safe during VM setup.
        unsafe {
            let _ = vaddr;
            asm!("invtlb 0x0, $r0, $r0");
            asm!("dbar 0");
        }
    }
}

#[derive(Copy, Clone)]
pub struct LoongArchPagingMetaDataL4;

impl ptg::TableMeta for LoongArchPagingMetaDataL4 {
    type P = LoongArchPTE;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        LoongArchPagingMetaDataL3::flush(vaddr);
    }
}

pub(crate) type NestedPageTable<H> = crate::arch::npt::LeveledPageTable<
    LoongArchPagingMetaDataL3,
    LoongArchPagingMetaDataL4,
    H,
    true,
>;

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
