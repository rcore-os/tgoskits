// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use core::marker::PhantomData;

use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_memory_set::{MappingError, MappingResult};
use axaddrspace::{NestedPageTableOps, PageSize};
use axvm_types::{GuestPhysAddr, MappingFlags};
use page_table_generic as ptg;

use crate::{AxVmError, AxVmResult, ax_err, host::PagingHandler};

struct GenericFrameAllocator<H>(PhantomData<fn() -> H>);

impl<H> GenericFrameAllocator<H> {
    const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<H> Clone for GenericFrameAllocator<H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H> Copy for GenericFrameAllocator<H> {}

impl<H: PagingHandler + 'static> ptg::FrameAllocator for GenericFrameAllocator<H> {
    fn alloc_frame(&self) -> Option<ptg::PhysAddr> {
        H::alloc_frame().map(|paddr| ptg::PhysAddr::new(paddr.as_usize()))
    }

    fn dealloc_frame(&self, frame: ptg::PhysAddr) {
        H::dealloc_frame(PhysAddr::from(frame.raw()));
    }

    fn phys_to_virt(&self, paddr: ptg::PhysAddr) -> *mut u8 {
        H::phys_to_virt(PhysAddr::from(paddr.raw())).as_usize() as *mut u8
    }

    fn alloc_frames(&self, frames: usize, align: usize) -> Option<ptg::PhysAddr> {
        H::alloc_frames(frames, align).map(|paddr| ptg::PhysAddr::new(paddr.as_usize()))
    }

    fn dealloc_frames(&self, start: ptg::PhysAddr, frames: usize, _frame_size: usize) {
        H::dealloc_frames(PhysAddr::from(start.raw()), frames);
    }
}

pub(crate) struct GenericNestedPageTable<M, P, H>
where
    M: ptg::TableMeta<P = P>,
    P: ptg::PageTableEntry,
    H: PagingHandler + 'static,
{
    inner: ptg::PageTable<M, GenericFrameAllocator<H>>,
}

impl<M, P, H> GenericNestedPageTable<M, P, H>
where
    M: ptg::TableMeta<P = P>,
    P: ptg::PageTableEntry,
    H: PagingHandler + 'static,
{
    pub(crate) fn try_new() -> ptg::PagingResult<Self> {
        Ok(Self {
            inner: ptg::PageTable::new(GenericFrameAllocator::new())?,
        })
    }

    pub(crate) fn root_paddr(&self) -> PhysAddr {
        PhysAddr::from(self.inner.root_paddr().raw())
    }

    pub(crate) fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> ptg::PagingResult {
        self.inner.map(&ptg::MapConfig {
            vaddr: ptg::VirtAddr::new(vaddr.as_usize()),
            paddr: ptg::PhysAddr::new(paddr.as_usize()),
            size: size.into(),
            pte: flags_to_config(flags),
            allow_huge: size.is_huge(),
            flush: true,
        })
    }

    pub(crate) fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> ptg::PagingResult {
        let paddr = get_paddr(vaddr);
        self.inner.map(&ptg::MapConfig {
            vaddr: ptg::VirtAddr::new(vaddr.as_usize()),
            paddr: ptg::PhysAddr::new(paddr.as_usize()),
            size,
            pte: flags_to_config(flags),
            allow_huge,
            flush: true,
        })
    }

    pub(crate) fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> ptg::PagingResult<(PhysAddr, MappingFlags, PageSize)> {
        let (paddr, flags, page_size) = self.query(vaddr)?;
        self.inner
            .unmap(ptg::VirtAddr::new(vaddr.as_usize()), page_size.into())?;
        Ok((paddr, flags, page_size))
    }

    pub(crate) fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> ptg::PagingResult {
        self.inner.unmap(ptg::VirtAddr::new(start.as_usize()), size)
    }

    pub(crate) fn remap(
        &mut self,
        start: GuestPhysAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
    ) -> ptg::PagingResult {
        let start = GuestPhysAddr::from(start.as_usize() & !(ax_memory_addr::PAGE_SIZE_4K - 1));
        let _ = self.unmap(start);
        self.map(start, paddr, PageSize::Size4K, flags)
    }

    pub(crate) fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> ptg::PagingResult {
        let mut vaddr = start;
        let end = start + size;
        while vaddr < end {
            let (paddr, _, page_size) = self.query(vaddr)?;
            self.inner
                .unmap(ptg::VirtAddr::new(vaddr.as_usize()), page_size.into())?;
            self.map(vaddr, paddr, page_size, new_flags)?;
            vaddr += usize::from(page_size);
        }
        Ok(())
    }

    pub(crate) fn query(
        &self,
        vaddr: GuestPhysAddr,
    ) -> ptg::PagingResult<(PhysAddr, MappingFlags, PageSize)> {
        let (paddr, pte, level) = self
            .inner
            .translate_with_level(ptg::VirtAddr::new(vaddr.as_usize()))?;
        let config = pte.to_config(level > 1);
        Ok((
            PhysAddr::from(paddr.raw()),
            config_to_flags(config),
            page_size_for_level::<M, GenericFrameAllocator<H>>(level, config.huge),
        ))
    }
}

pub(crate) enum LeveledPageTable<M3, M4, H, const SUPPORT_L3: bool>
where
    M3: ptg::TableMeta,
    M4: ptg::TableMeta,
    H: PagingHandler + 'static,
{
    L3(GenericNestedPageTable<M3, M3::P, H>),
    L4(GenericNestedPageTable<M4, M4::P, H>),
}

impl<M3, M4, H, const SUPPORT_L3: bool> LeveledPageTable<M3, M4, H, SUPPORT_L3>
where
    M3: ptg::TableMeta,
    M4: ptg::TableMeta,
    H: PagingHandler + 'static,
{
    pub(crate) fn new(level: usize) -> AxVmResult<Self> {
        match level {
            3 => {
                if !SUPPORT_L3 {
                    return ax_err!(InvalidInput, "L3 not supported on this architecture");
                }
                let table = GenericNestedPageTable::try_new().map_err(map_new_error)?;
                Ok(Self::L3(table))
            }
            4 => {
                let table = GenericNestedPageTable::try_new().map_err(map_new_error)?;
                Ok(Self::L4(table))
            }
            _ => ax_err!(InvalidInput, "Invalid page table level"),
        }
    }

    pub(crate) fn root_paddr(&self) -> PhysAddr {
        match self {
            Self::L3(pt) => pt.root_paddr(),
            Self::L4(pt) => pt.root_paddr(),
        }
    }

    pub(crate) const fn levels(&self) -> usize {
        match self {
            Self::L3(_) => 3,
            Self::L4(_) => 4,
        }
    }

    pub(crate) fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> MappingResult {
        match self {
            Self::L3(pt) => pt.map(vaddr, paddr, size, flags),
            Self::L4(pt) => pt.map(vaddr, paddr, size, flags),
        }
        .map_err(map_error)
    }

    pub(crate) fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> MappingResult<(PhysAddr, MappingFlags, PageSize)> {
        match self {
            Self::L3(pt) => pt.unmap(vaddr),
            Self::L4(pt) => pt.unmap(vaddr),
        }
        .map_err(map_error)
    }

    pub(crate) fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> MappingResult {
        match self {
            Self::L3(pt) => pt.map_region(vaddr, &get_paddr, size, flags, allow_huge),
            Self::L4(pt) => pt.map_region(vaddr, &get_paddr, size, flags, allow_huge),
        }
        .map_err(map_error)
    }

    pub(crate) fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> MappingResult {
        match self {
            Self::L3(pt) => pt.unmap_region(start, size),
            Self::L4(pt) => pt.unmap_region(start, size),
        }
        .map_err(map_error)
    }

    pub(crate) fn remap(
        &mut self,
        start: GuestPhysAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
    ) -> bool {
        match self {
            Self::L3(pt) => pt.remap(start, paddr, flags),
            Self::L4(pt) => pt.remap(start, paddr, flags),
        }
        .is_ok()
    }

    pub(crate) fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> bool {
        match self {
            Self::L3(pt) => pt.protect_region(start, size, new_flags),
            Self::L4(pt) => pt.protect_region(start, size, new_flags),
        }
        .is_ok()
    }

    pub(crate) fn query(
        &self,
        vaddr: GuestPhysAddr,
    ) -> MappingResult<(PhysAddr, MappingFlags, PageSize)> {
        match self {
            Self::L3(pt) => pt.query(vaddr),
            Self::L4(pt) => pt.query(vaddr),
        }
        .map_err(map_error)
    }
}

impl<M3, M4, H, const SUPPORT_L3: bool> NestedPageTableOps
    for LeveledPageTable<M3, M4, H, SUPPORT_L3>
where
    M3: ptg::TableMeta,
    M4: ptg::TableMeta,
    H: PagingHandler + 'static,
{
    fn root_paddr(&self) -> PhysAddr {
        LeveledPageTable::root_paddr(self)
    }

    fn levels(&self) -> usize {
        LeveledPageTable::levels(self)
    }

    fn alloc_frame(&self) -> Option<PhysAddr> {
        H::alloc_frame()
    }

    fn dealloc_frame(&self, paddr: PhysAddr) {
        H::dealloc_frame(paddr);
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
        H::phys_to_virt(paddr)
    }

    fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> MappingResult {
        LeveledPageTable::map(self, vaddr, paddr, size, flags)
    }

    fn unmap(&mut self, vaddr: GuestPhysAddr) -> MappingResult<(PhysAddr, MappingFlags, PageSize)> {
        LeveledPageTable::unmap(self, vaddr)
    }

    fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> MappingResult {
        LeveledPageTable::map_region(self, vaddr, get_paddr, size, flags, allow_huge)
    }

    fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> MappingResult {
        LeveledPageTable::unmap_region(self, start, size)
    }

    fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool {
        LeveledPageTable::remap(self, start, paddr, flags)
    }

    fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> bool {
        LeveledPageTable::protect_region(self, start, size, new_flags)
    }

    fn query(&self, vaddr: GuestPhysAddr) -> MappingResult<(PhysAddr, MappingFlags, PageSize)> {
        LeveledPageTable::query(self, vaddr)
    }
}

fn flags_to_config(flags: MappingFlags) -> ptg::PteConfig {
    ptg::PteConfig {
        valid: !flags.is_empty(),
        read: flags.contains(MappingFlags::READ),
        writable: flags.contains(MappingFlags::WRITE),
        executable: flags.contains(MappingFlags::EXECUTE),
        lower: flags.contains(MappingFlags::USER),
        mem_attr: if flags.contains(MappingFlags::DEVICE) {
            ptg::MemAttributes::Device
        } else if flags.contains(MappingFlags::UNCACHED) {
            ptg::MemAttributes::Uncached
        } else {
            ptg::MemAttributes::Normal
        },
        ..Default::default()
    }
}

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

fn page_size_for_level<M, A>(level: usize, huge: bool) -> PageSize
where
    M: ptg::TableMeta,
    A: ptg::FrameAllocator,
{
    if !huge {
        return PageSize::Size4K;
    }
    match ptg::Frame::<M, A>::level_size(level) {
        0x10_0000 => PageSize::Size1M,
        0x20_0000 => PageSize::Size2M,
        0x4000_0000 => PageSize::Size1G,
        _ => PageSize::Size4K,
    }
}

pub(crate) fn map_new_error(err: ptg::PagingError) -> AxVmError {
    match err {
        ptg::PagingError::NoMemory => AxVmError::OutOfMemory {
            operation: "allocate nested page table",
        },
        _ => AxVmError::memory("create nested page table", err),
    }
}

pub(crate) fn map_error(err: ptg::PagingError) -> MappingError {
    match err {
        ptg::PagingError::MappingConflict { .. } => MappingError::AlreadyExists,
        ptg::PagingError::AlignmentError { .. }
        | ptg::PagingError::AddressOverflow { .. }
        | ptg::PagingError::InvalidSize { .. }
        | ptg::PagingError::InvalidRange { .. } => MappingError::InvalidParam,
        ptg::PagingError::NoMemory
        | ptg::PagingError::HierarchyError { .. }
        | ptg::PagingError::NotMapped => MappingError::BadState,
    }
}
