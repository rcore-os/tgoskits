// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

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
        // SAFETY: `hfence.gvma` only orders guest-stage translations. It does
        // not access memory directly and is required after G-stage PTE updates.
        unsafe {
            std::arch::asm!(
                ".option push",
                ".option arch, +h",
                "hfence.gvma",
                ".option pop",
                options(nostack, preserves_flags),
            );
        }
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

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct RiscvPte(usize);

impl RiscvPte {
    const V: usize = 1 << 0;
    const R: usize = 1 << 1;
    const W: usize = 1 << 2;
    const X: usize = 1 << 3;
    const U: usize = 1 << 4;
    const A: usize = 1 << 6;
    const D: usize = 1 << 7;
    const PPN_MASK: usize = (1usize << 54) - (1usize << 10);
}

impl ptg::PageTableEntry for RiscvPte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: ptg::PhysAddr, config: Self::PteConfig, _is_huge: bool) -> Self {
        if config.is_empty() {
            return Self(0);
        }

        let mut bits = (paddr.as_usize() >> 2) & Self::PPN_MASK;
        bits |= Self::V;
        if config.contains(MappingFlags::READ) {
            bits |= Self::R;
        }
        if config.contains(MappingFlags::WRITE) {
            bits |= Self::W | Self::R;
        }
        if config.contains(MappingFlags::EXECUTE) {
            bits |= Self::X;
        }
        if config.contains(MappingFlags::USER) {
            bits |= Self::U;
        }
        bits |= Self::A | Self::D;
        Self(bits)
    }

    fn new_table(paddr: ptg::PhysAddr) -> Self {
        Self(((paddr.as_usize() >> 2) & Self::PPN_MASK) | Self::V)
    }

    fn paddr(&self, _is_dir: bool) -> ptg::PhysAddr {
        ptg::PhysAddr::from_usize((self.0 & Self::PPN_MASK) << 2)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.0;
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, flags & Self::R != 0);
        config.set(MappingFlags::WRITE, flags & Self::W != 0);
        config.set(MappingFlags::EXECUTE, flags & Self::X != 0);
        config.set(MappingFlags::USER, flags & Self::U != 0);
        config
    }

    fn present(&self) -> bool {
        self.0 & Self::V != 0
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.0 & (Self::R | Self::W | Self::X) != 0
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<Sv39x4MetaData, Sv48x4MetaData, H, true>;
