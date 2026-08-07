mod mocks;

use std::alloc::{self, Layout};

use mocks::{MappingFlags, PteImpl};
use page_table_generic::{FrameAllocator, PageTable, PhysAddr, TableMeta, VirtAddr};

const PAGE_SIZE_16K: usize = 0x4000;
const BLOCK_SIZE_32M: usize = PAGE_SIZE_16K << 11;

#[derive(Clone, Copy)]
struct T16kL4;

impl TableMeta for T16kL4 {
    type P = PteImpl;

    const PAGE_SIZE: usize = PAGE_SIZE_16K;
    const LEVEL_BITS: &[usize] = &[11, 11, 11, 11];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<VirtAddr>) {}
}

#[derive(Clone, Copy)]
struct Fram16k;

impl FrameAllocator for Fram16k {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(PAGE_SIZE_16K, PAGE_SIZE_16K).unwrap();
        // SAFETY: the layout has a non-zero size and valid page alignment.
        let ptr = unsafe { alloc::alloc(layout) };
        (!ptr.is_null()).then(|| PhysAddr::from_usize(ptr as usize))
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        let layout = Layout::from_size_align(PAGE_SIZE_16K, PAGE_SIZE_16K).unwrap();
        // SAFETY: every frame was allocated with this exact layout and is released once.
        unsafe { alloc::dealloc(frame.as_usize() as *mut u8, layout) };
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.as_usize() as *mut u8
    }
}

#[test]
fn query_reports_arbitrary_base_page_size() {
    let mut page_table = PageTable::<T16kL4, Fram16k>::new(Fram16k).unwrap();
    let vaddr = VirtAddr::from_usize(PAGE_SIZE_16K);
    let paddr = PhysAddr::from_usize(PAGE_SIZE_16K);

    page_table
        .map_page(vaddr, paddr, PAGE_SIZE_16K, MappingFlags::READ.into())
        .unwrap();

    let (mapped_paddr, _, page_size) = page_table.query(vaddr).unwrap();
    assert_eq!(mapped_paddr, paddr);
    assert_eq!(page_size, PAGE_SIZE_16K);
}

#[test]
fn map_region_selects_page_sizes_from_table_levels() {
    let mut page_table = PageTable::<T16kL4, Fram16k>::new(Fram16k).unwrap();
    let vaddr = VirtAddr::from_usize(BLOCK_SIZE_32M);
    let paddr = PhysAddr::from_usize(BLOCK_SIZE_32M);

    page_table
        .map_region(
            vaddr,
            |current| paddr + (current - vaddr),
            BLOCK_SIZE_32M,
            MappingFlags::READ.into(),
            true,
        )
        .unwrap();

    let (mapped_paddr, _, page_size) = page_table.query(vaddr + PAGE_SIZE_16K).unwrap();
    assert_eq!(mapped_paddr, paddr + PAGE_SIZE_16K);
    assert_eq!(page_size, BLOCK_SIZE_32M);
}
