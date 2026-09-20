// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use ax_cpu::paging::{DescriptorFlags, PageTableEntry, Pte};
use axvm_types::MappingFlags;
use page_table_generic as ptg;

#[derive(Clone, Copy)]
pub struct Sv39x4MetaData;

impl ptg::TableMeta for Sv39x4MetaData {
    type P = RiscvPte;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[11, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(_vaddr: Option<ptg::VirtAddr>) {
        // The VM owner retires all guests before mutation; the CPU entry
        // fences G-stage translations on every later entry, including reentry
        // within an existing VS register-bank binding.
    }
}

#[derive(Clone, Copy)]
pub struct Sv48x4MetaData;

impl ptg::TableMeta for Sv48x4MetaData {
    type P = RiscvPte;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[11, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(_vaddr: Option<ptg::VirtAddr>) {
        Sv39x4MetaData::flush(_vaddr);
    }
}

/// VM mapping policy over the CPU-owned Sv descriptor encoding.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct RiscvPte(Pte);

impl PageTableEntry for RiscvPte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: ptg::PhysAddr, config: MappingFlags, _is_huge: bool) -> Self {
        if config.is_empty() {
            return Self(Pte::default());
        }
        // G-stage uses the same descriptor layout as S-stage. The VM selects
        // its own A/D and user-access policy, without S-stage vendor defaults.
        let mut flags = DescriptorFlags::V | DescriptorFlags::A | DescriptorFlags::D;
        flags.set(
            DescriptorFlags::R,
            config.intersects(MappingFlags::READ | MappingFlags::WRITE),
        );
        flags.set(DescriptorFlags::W, config.contains(MappingFlags::WRITE));
        flags.set(DescriptorFlags::X, config.contains(MappingFlags::EXECUTE));
        flags.set(DescriptorFlags::U, config.contains(MappingFlags::USER));
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: ptg::PhysAddr) -> Self {
        Self(Pte::from_parts(paddr, DescriptorFlags::V))
    }

    fn paddr(&self, is_dir: bool) -> ptg::PhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, _is_dir: bool) -> MappingFlags {
        let flags = self.0.flags();
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, flags.contains(DescriptorFlags::R));
        config.set(MappingFlags::WRITE, flags.contains(DescriptorFlags::W));
        config.set(MappingFlags::EXECUTE, flags.contains(DescriptorFlags::X));
        config.set(MappingFlags::USER, flags.contains(DescriptorFlags::U));
        config
    }

    fn present(&self) -> bool {
        self.0.present()
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir
            && self
                .0
                .flags()
                .intersects(DescriptorFlags::R | DescriptorFlags::W | DescriptorFlags::X)
    }

    fn unused(&self) -> bool {
        self.0.unused()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<Sv39x4MetaData, Sv48x4MetaData, H, true>;
