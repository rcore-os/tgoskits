use core::sync::atomic::Ordering;

use ax_memory_addr::{PAGE_SIZE_4K as PAGE_SIZE, PhysAddr, VirtAddr};
use ax_memory_set::MappingError;
use axaddrspace::{AddrSpaceError, AddrSpaceResult, MappingFlags, NestedPageTableOps, PageSize};
use axvm_types::GuestPhysAddr;
use page_table_generic as ptg;
use ptg::PageTableEntry;

use crate::test_utils::{
    ALLOC_COUNT, ALLOC_SHOULD_FAIL, BASE_PADDR, DEALLOC_COUNT, DEALLOCATED_FRAMES, MEMORY_LEN,
    MockHal, NEXT_PADDR,
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

fn mock_dealloc_frame(paddr: PhysAddr) {
    DEALLOCATED_FRAMES.lock().unwrap().push(paddr.as_usize());
    DEALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
struct MockAllocator;

impl ptg::PageFrameProvider for MockAllocator {
    fn alloc_frame(&self) -> Option<ptg::PhysAddr> {
        mock_alloc_frame()
    }

    fn dealloc_frame(&self, frame: ptg::PhysAddr) {
        mock_dealloc_frame(frame);
    }

    fn phys_to_virt(&self, paddr: ptg::PhysAddr) -> ptg::VirtAddr {
        MockHal::mock_phys_to_virt(paddr)
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

    fn flags_to_config(flags: MappingFlags, paddr: PhysAddr) -> ptg::PteConfig {
        if flags.is_empty() && paddr.as_usize() == 0 {
            return ptg::PteConfig::default();
        }
        ptg::PteConfig {
            valid: true,
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
}

impl PageTableEntry for MockPte {
    fn from_config(config: ptg::PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        let mut bits = config.paddr.as_usize() & Self::PPN_MASK;
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
            bits |= Self::D;
        }
        Self(bits)
    }

    fn to_config(&self, is_dir: bool) -> ptg::PteConfig {
        let leaf = self.0 & (Self::R | Self::W | Self::X) != 0;
        ptg::PteConfig {
            paddr: ptg::PhysAddr::from_usize(self.0 & Self::PPN_MASK),
            valid: self.0 & Self::V != 0,
            read: self.0 & Self::R != 0,
            writable: self.0 & Self::W != 0,
            executable: self.0 & Self::X != 0,
            lower: self.0 & Self::U != 0,
            dirty: self.0 & Self::D != 0,
            is_dir: is_dir && !leaf,
            huge: is_dir && leaf,
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        self.0 & Self::V != 0
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
        PhysAddr::from(self.inner.root_paddr().as_usize())
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
                paddr: ptg::PhysAddr::from_usize(paddr.as_usize()),
                size: size.into(),
                pte: MockPte::flags_to_config(flags, paddr),
                allow_huge: size.is_huge(),
                flush: false,
            })
            .map_err(Self::convert_err)?)
    }

    fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)> {
        let (paddr, flags, page_size) = self.query(vaddr)?;
        self.inner
            .unmap(
                ptg::VirtAddr::from_usize(vaddr.as_usize()),
                page_size.into(),
            )
            .map_err(Self::convert_err)?;
        Ok((paddr, flags, page_size))
    }

    fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> AddrSpaceResult {
        let paddr = get_paddr(vaddr);
        Ok(self
            .inner
            .map(&ptg::MapConfig {
                vaddr: ptg::VirtAddr::from_usize(vaddr.as_usize()),
                paddr: ptg::PhysAddr::from_usize(paddr.as_usize()),
                size,
                pte: MockPte::flags_to_config(flags, paddr),
                allow_huge,
                flush: false,
            })
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
        let (paddr, pte, level) = self
            .inner
            .translate_with_level(ptg::VirtAddr::from_usize(vaddr.as_usize()))
            .map_err(|err| match err {
                ptg::PagingError::NotMapped => AddrSpaceError::Unmapped { address: vaddr },
                _ => AddrSpaceError::MappingState,
            })?;
        let page_size = match level {
            1 => PageSize::Size4K,
            2 => PageSize::Size2M,
            3 => PageSize::Size1G,
            _ => return Err(AddrSpaceError::MappingState),
        };
        let config = pte.to_config(page_size.is_huge());
        if !config.valid || MockPte::config_to_flags(config).is_empty() {
            return Err(AddrSpaceError::Unmapped { address: vaddr });
        }
        Ok((
            PhysAddr::from(paddr.as_usize()),
            MockPte::config_to_flags(config),
            page_size,
        ))
    }
}
