// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use ax_cpu::paging::{DescriptorFlags, PageTableEntry, Pte};
use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

fn descriptor_flags(config: MappingFlags, is_huge: bool) -> DescriptorFlags {
    let mut flags = DescriptorFlags::V | DescriptorFlags::P;
    flags.set(DescriptorFlags::NR, !config.contains(MappingFlags::READ));
    if config.contains(MappingFlags::WRITE) {
        flags |= DescriptorFlags::W | DescriptorFlags::D;
    }
    flags.set(DescriptorFlags::NX, !config.contains(MappingFlags::EXECUTE));
    if config.contains(MappingFlags::USER) {
        flags |= DescriptorFlags::PLVL | DescriptorFlags::PLVH;
    }
    if !config.contains(MappingFlags::DEVICE) {
        flags |= if config.contains(MappingFlags::UNCACHED) {
            DescriptorFlags::MATH
        } else {
            DescriptorFlags::MATL
        };
    }
    flags.set(DescriptorFlags::GH, is_huge);
    flags
}

fn mapping_flags(flags: DescriptorFlags) -> MappingFlags {
    if !flags.contains(DescriptorFlags::V) {
        return MappingFlags::empty();
    }
    let mut config = MappingFlags::empty();
    config.set(MappingFlags::READ, !flags.contains(DescriptorFlags::NR));
    config.set(MappingFlags::WRITE, flags.contains(DescriptorFlags::W));
    config.set(MappingFlags::EXECUTE, !flags.contains(DescriptorFlags::NX));
    config.set(
        MappingFlags::USER,
        flags.contains(DescriptorFlags::PLVL | DescriptorFlags::PLVH),
    );
    if !flags.contains(DescriptorFlags::MATL) {
        config |= if flags.contains(DescriptorFlags::MATH) {
            MappingFlags::UNCACHED
        } else {
            MappingFlags::DEVICE
        };
    }
    config
}

/// VM table policy over the CPU-owned LoongArch descriptor encoding.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct LoongArchPTE(Pte);

impl PageTableEntry for LoongArchPTE {
    type PteConfig = MappingFlags;

    fn new_page(paddr: HostPhysAddr, config: MappingFlags, is_huge: bool) -> Self {
        if config.is_empty() {
            return Self(Pte::default());
        }
        Self(Pte::from_parts(paddr, descriptor_flags(config, is_huge)))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        // Retain the guest table owner's directory convention, independently
        // of the address-only directories selected by the native walker.
        Self(Pte::from_parts(
            paddr,
            DescriptorFlags::V | DescriptorFlags::P | DescriptorFlags::MATL,
        ))
    }

    fn paddr(&self, is_dir: bool) -> HostPhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, _is_dir: bool) -> MappingFlags {
        mapping_flags(self.0.flags())
    }

    fn present(&self) -> bool {
        self.0.flags().contains(DescriptorFlags::V)
    }

    fn huge(&self, is_dir: bool) -> bool {
        self.0.huge(is_dir)
    }

    fn unused(&self) -> bool {
        self.0.unused()
    }

    fn clear(&mut self) {
        self.0.clear();
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

    fn flush(_vaddr: Option<ptg::VirtAddr>) {
        // Tables are mutated with guest admission closed. Every subsequent
        // guest entry invalidates that CPU's guest-tagged translation domain.
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

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<LoongArchPagingMetaDataL3, LoongArchPagingMetaDataL4, H, true>;
