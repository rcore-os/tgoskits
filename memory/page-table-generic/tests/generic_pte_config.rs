use core::{alloc::Layout, fmt};

use page_table_generic::{
    FrameAllocator, PageSize, PageTable, PageTableEntry, PhysAddr, TableMeta, VirtAddr,
};

#[derive(Clone, Copy)]
struct TestAllocator;

impl FrameAllocator for TestAllocator {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        self.alloc_frames(1, 0x1000)
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        self.dealloc_frames(frame, 1, 0x1000);
    }

    fn alloc_frames(&self, frames: usize, align: usize) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(0x1000 * frames, align).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        (!ptr.is_null()).then(|| PhysAddr::from_usize(ptr as usize))
    }

    fn dealloc_frames(&self, start: PhysAddr, frames: usize, _frame_size: usize) {
        let layout = Layout::from_size_align(0x1000 * frames, 0x1000 * frames).unwrap();
        unsafe { std::alloc::dealloc(start.as_usize() as *mut u8, layout) };
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.as_usize() as *mut u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpaqueConfig {
    domain: u8,
}

#[derive(Clone, Copy)]
struct OpaquePte(u64);

impl OpaquePte {
    const PRESENT: u64 = 1;
    const TABLE: u64 = 1 << 1;
    const HUGE: u64 = 1 << 2;
    const CONFIG_SHIFT: usize = 3;
    const CONFIG_MASK: u64 = 0xff << Self::CONFIG_SHIFT;
    const PADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
}

impl PageTableEntry for OpaquePte {
    type PteConfig = OpaqueConfig;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        let huge = if is_huge { Self::HUGE } else { 0 };
        Self(
            (paddr.as_usize() as u64 & Self::PADDR_MASK)
                | Self::PRESENT
                | huge
                | (u64::from(config.domain) << Self::CONFIG_SHIFT),
        )
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self((paddr.as_usize() as u64 & Self::PADDR_MASK) | Self::PRESENT | Self::TABLE)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        OpaqueConfig {
            domain: ((self.0 & Self::CONFIG_MASK) >> Self::CONFIG_SHIFT) as u8,
        }
    }

    fn present(&self) -> bool {
        self.0 & Self::PRESENT != 0
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.0 & Self::HUGE != 0
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl fmt::Debug for OpaquePte {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OpaquePte").field(&self.0).finish()
    }
}

#[derive(Clone, Copy)]
struct OpaqueMeta;

impl TableMeta for OpaqueMeta {
    type P = OpaquePte;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;

    fn flush(_vaddr: Option<VirtAddr>) {}
}

#[test]
fn maps_and_protects_with_an_opaque_pte_config() {
    let mut page_table = PageTable::<OpaqueMeta, TestAllocator>::new(TestAllocator).unwrap();
    let vaddr = VirtAddr::from_usize(0x4000);
    let paddr = PhysAddr::from_usize(0x8000);

    page_table
        .map_page(
            vaddr,
            paddr,
            PageSize::Size4K,
            OpaqueConfig { domain: 0x2a },
        )
        .unwrap();
    assert_eq!(page_table.query(vaddr).unwrap().1.domain, 0x2a);

    page_table
        .protect_page(vaddr, OpaqueConfig { domain: 0x7f })
        .unwrap();
    assert_eq!(page_table.query(vaddr).unwrap().1.domain, 0x7f);
}
