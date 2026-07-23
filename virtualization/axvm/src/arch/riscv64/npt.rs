// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use ax_page_table::stage2 as ptg;

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
            core::arch::asm!("hfence.gvma", options(nostack, preserves_flags));
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
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }

        let mut bits = (config.paddr.as_usize() >> 2) & Self::PPN_MASK;
        bits |= Self::V;
        if !config.is_dir || config.huge {
            if config.read {
                bits |= Self::R;
            }
            if config.writable {
                bits |= Self::W | Self::R;
            }
            if config.executable {
                bits |= Self::X;
            }
            if config.lower {
                bits |= Self::U;
            }
            bits |= Self::A | Self::D;
        }
        Self(bits)
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let flags = self.0;
        let leaf = flags & (Self::R | Self::W | Self::X) != 0;
        ptg::PteConfig {
            paddr: ptg::PhysAddr::from_usize((flags & Self::PPN_MASK) << 2),
            valid: flags & Self::V != 0,
            read: flags & Self::R != 0,
            writable: flags & Self::W != 0,
            executable: flags & Self::X != 0,
            lower: flags & Self::U != 0,
            dirty: flags & Self::D != 0,
            is_dir: is_dir && !leaf,
            huge: is_dir && leaf,
            mem_attr: ptg::MemAttributes::Normal,
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        self.0 & Self::V != 0
    }
}

pub(crate) type NestedPageTable<H> =
    crate::npt::LeveledPageTable<Sv39x4MetaData, Sv48x4MetaData, H, true>;
