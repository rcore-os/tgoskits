// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

use ax_cpu::paging::{MappingFlags as CpuMappingFlags, PageTableEntry, Stage2Pte};
use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

fn cpu_flags(flags: MappingFlags) -> CpuMappingFlags {
    let mut result = CpuMappingFlags::empty();
    result.set(CpuMappingFlags::READ, flags.contains(MappingFlags::READ));
    result.set(CpuMappingFlags::WRITE, flags.contains(MappingFlags::WRITE));
    result.set(
        CpuMappingFlags::EXECUTE,
        flags.contains(MappingFlags::EXECUTE),
    );
    result.set(CpuMappingFlags::USER, flags.contains(MappingFlags::USER));
    result.set(
        CpuMappingFlags::DEVICE,
        flags.contains(MappingFlags::DEVICE),
    );
    result.set(
        CpuMappingFlags::UNCACHED,
        flags.contains(MappingFlags::UNCACHED),
    );
    result
}

fn vm_flags(flags: CpuMappingFlags) -> MappingFlags {
    let mut result = MappingFlags::empty();
    result.set(MappingFlags::READ, flags.contains(CpuMappingFlags::READ));
    result.set(MappingFlags::WRITE, flags.contains(CpuMappingFlags::WRITE));
    result.set(
        MappingFlags::EXECUTE,
        flags.contains(CpuMappingFlags::EXECUTE),
    );
    result.set(
        MappingFlags::DEVICE,
        flags.contains(CpuMappingFlags::DEVICE),
    );
    result.set(
        MappingFlags::UNCACHED,
        flags.contains(CpuMappingFlags::UNCACHED),
    );
    result
}

/// VM policy boundary for the CPU-owned stage-two descriptor.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct A64PTEHV(Stage2Pte);

impl PageTableEntry for A64PTEHV {
    type PteConfig = MappingFlags;

    fn new_page(paddr: HostPhysAddr, config: MappingFlags, is_huge: bool) -> Self {
        Self(Stage2Pte::new_page(paddr, cpu_flags(config), is_huge))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        Self(Stage2Pte::new_table(paddr))
    }

    fn paddr(&self, is_dir: bool) -> HostPhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, is_dir: bool) -> MappingFlags {
        vm_flags(self.0.config(is_dir))
    }

    fn present(&self) -> bool {
        self.0.present()
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
pub struct A64HVPagingMetaDataL3;

impl ptg::TableMeta for A64HVPagingMetaDataL3 {
    type P = A64PTEHV;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(_vaddr: Option<ptg::VirtAddr>) {
        // SAFETY: AxVM owns these stage-two tables at EL2 and serializes table
        // mutation. The static walker callback carries no VMID, so invalidate
        // all guest contexts in the shareable domain before retiring entries.
        unsafe { ax_cpu::virtualization::invalidate_guest_translations_inner_shareable() };
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
