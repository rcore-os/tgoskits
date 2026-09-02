use core::sync::atomic::Ordering;

use ax_memory_addr::{PAGE_SIZE_4K as PAGE_SIZE, PhysAddr, VirtAddr};
use ax_memory_set::MappingError;
use axaddrspace::{AddrSpaceError, AddrSpaceResult, MappingFlags, NestedPageTableOps, PageSize};
use axvm_types::GuestPhysAddr;
use page_table_generic as ptg;
use ptg::PageTableEntry;

use crate::test_utils::{
    ALLOC_COUNT, ALLOC_SHOULD_FAIL, BASE_PADDR, DEALLOC_COUNT, MEMORY_LEN, MockHal, NEXT_PADDR,
};

fn mock_alloc_frame() -> Option<PhysAddr> {
    if ALLOC_SHOULD_FAIL.load(Ordering::SeqCst) {
        return None;
    }

    let paddr = NEXT_PADDR.fetch_add(PAGE_SIZE, Ordering::SeqCst);
    if paddr >= MEMORY_LEN + BASE_PADDR {
        return None;
    }
    ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
    Some(PhysAddr::from_usize(paddr))
}

fn mock_dealloc_frame(_paddr: PhysAddr) {
    DEALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
struct MockAllocator;

impl ptg::FrameAllocator for MockAllocator {
    fn alloc_frame(&self) -> Option<ptg::PhysAddr> {
        mock_alloc_frame()
    }

    fn dealloc_frame(&self, frame: ptg::PhysAddr) {
        mock_dealloc_frame(frame);
    }

    fn phys_to_virt(&self, paddr: ptg::PhysAddr) -> *mut u8 {
        MockHal::mock_phys_to_virt(paddr).as_usize() as *mut u8
    }
}

#[derive(Clone, Copy)]
struct MockMeta;

impl ptg::TableMeta for MockMeta {
    type P = MockPte;

    const PAGE_SIZE: usize = PAGE_SIZE;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<ptg::VirtAddr>) {}
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
struct MockPte(usize);

impl MockPte {
    const V: usize = 1 << 0;
    const R: usize = 1 << 1;
    const W: usize = 1 << 2;
    const X: usize = 1 << 3;
    const U: usize = 1 << 4;
    const D: usize = 1 << 7;
    const PPN_MASK: usize = !0xfff;
}

impl PageTableEntry for MockPte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, _is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        let mut bits = paddr.as_usize() & Self::PPN_MASK;
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
        bits |= Self::D;
        Self(bits)
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self((paddr.as_usize() & Self::PPN_MASK) | Self::V)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize(self.0 & Self::PPN_MASK)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let mut flags = MappingFlags::empty();
        flags.set(MappingFlags::READ, self.0 & Self::R != 0);
        flags.set(MappingFlags::WRITE, self.0 & Self::W != 0);
        flags.set(MappingFlags::EXECUTE, self.0 & Self::X != 0);
        flags.set(MappingFlags::USER, self.0 & Self::U != 0);
        flags
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

pub struct MockNestedPageTable {
    inner: ptg::PageTable<MockMeta, MockAllocator>,
}

impl MockNestedPageTable {
    pub fn new() -> Self {
        Self {
            inner: ptg::PageTable::new(MockAllocator).unwrap(),
        }
    }

    fn convert_err(_err: ptg::PagingError) -> MappingError {
        MappingError::BadState
    }
}

impl NestedPageTableOps for MockNestedPageTable {
    fn root_paddr(&self) -> PhysAddr {
        self.inner.root_paddr()
    }

    fn levels(&self) -> usize {
        4
    }

    fn alloc_frame(&self) -> Option<PhysAddr> {
        mock_alloc_frame()
    }

    fn dealloc_frame(&self, paddr: PhysAddr) {
        mock_dealloc_frame(paddr);
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
        MockHal::mock_phys_to_virt(paddr)
    }

    fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> AddrSpaceResult {
        Ok(self
            .inner
            .map(&ptg::MapConfig {
                vaddr: ptg::VirtAddr::from_usize(vaddr.as_usize()),
                paddr,
                size: size.into(),
                pte: flags,
                allow_huge: false,
                flush: false,
            })
            .map_err(Self::convert_err)?)
    }

    fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)> {
        let (paddr, flags, _) = self.query(vaddr)?;
        self.inner
            .unmap(ptg::VirtAddr::from_usize(vaddr.as_usize()), PAGE_SIZE)
            .map_err(Self::convert_err)?;
        Ok((paddr, flags, PageSize::Size4K))
    }

    fn map_linear(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> AddrSpaceResult {
        Ok(self
            .inner
            .map_linear_pages(
                ptg::VirtAddr::from_usize(vaddr.as_usize()),
                paddr,
                size,
                flags,
                allow_huge,
            )
            .map_err(Self::convert_err)?)
    }

    fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> AddrSpaceResult {
        Ok(self
            .inner
            .unmap(ptg::VirtAddr::from_usize(start.as_usize()), size)
            .map_err(Self::convert_err)?)
    }

    fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool {
        let start = GuestPhysAddr::from(start.as_usize() & !(PAGE_SIZE - 1));
        let _ = self.unmap(start);
        self.map(start, paddr, PageSize::Size4K, flags).is_ok()
    }

    fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> bool {
        let mut vaddr = start;
        let end = start + size;
        while vaddr < end {
            let Ok((paddr, ..)) = self.query(vaddr) else {
                return false;
            };
            let _ = self.unmap(vaddr);
            if self.map(vaddr, paddr, PageSize::Size4K, new_flags).is_err() {
                return false;
            }
            vaddr += PAGE_SIZE;
        }
        true
    }

    fn query(&self, vaddr: GuestPhysAddr) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)> {
        let (paddr, pte) = self
            .inner
            .translate(ptg::VirtAddr::from_usize(vaddr.as_usize()))
            .map_err(Self::convert_err)?;
        let flags = pte.config(false);
        if !pte.present() || flags.is_empty() {
            return Err(AddrSpaceError::MappingState);
        }
        Ok((paddr, flags, PageSize::Size4K))
    }
}
