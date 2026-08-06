//! TLB flush behavior for range operations.

#![cfg(not(target_os = "none"))]

use std::{
    alloc::{self, Layout},
    sync::atomic::{AtomicUsize, Ordering},
};

use page_table_generic::*;

const PRESENT: usize = 1 << 0;
const HUGE: usize = 1 << 1;
const PHYS_ADDR_MASK: usize = 0x000f_ffff_ffff_f000;

#[derive(Clone, Copy, Debug)]
struct TestPte(usize);

impl PageTableEntry for TestPte {
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        Self(
            (config.paddr.as_usize() & PHYS_ADDR_MASK)
                | PRESENT
                | if config.huge { HUGE } else { 0 },
        )
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let valid = self.valid();
        let huge = is_dir && self.0 & HUGE != 0;
        PteConfig {
            paddr: PhysAddr::from_usize(self.0 & PHYS_ADDR_MASK),
            valid,
            read: valid,
            writable: valid,
            is_dir: is_dir && valid && !huge,
            huge,
            ..Default::default()
        }
    }

    fn valid(&self) -> bool {
        self.0 & PRESENT != 0
    }
}

#[derive(Clone, Copy)]
struct TestAllocator;

impl FrameAllocator for TestAllocator {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(CountingMeta::PAGE_SIZE, CountingMeta::PAGE_SIZE)
            .expect("page layout must be valid");
        // SAFETY: `layout` has a non-zero size and page alignment.
        let ptr = unsafe { alloc::alloc(layout) };
        (!ptr.is_null()).then_some(PhysAddr::from_usize(ptr as usize))
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        let layout = Layout::from_size_align(CountingMeta::PAGE_SIZE, CountingMeta::PAGE_SIZE)
            .expect("page layout must be valid");
        // SAFETY: every frame returned by `alloc_frame` uses this exact layout
        // and page tables release each owned frame once.
        unsafe { alloc::dealloc(frame.as_usize() as *mut u8, layout) };
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.as_usize() as *mut u8
    }
}

static FULL_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static ADDRESS_FLUSHES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct CountingMeta;

impl TableMeta for CountingMeta {
    type P = TestPte;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<VirtAddr>) {
        if vaddr.is_some() {
            ADDRESS_FLUSHES.fetch_add(1, Ordering::Relaxed);
        } else {
            FULL_FLUSHES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[test]
fn map_region_batches_tlb_flushes() {
    FULL_FLUSHES.store(0, Ordering::Relaxed);
    ADDRESS_FLUSHES.store(0, Ordering::Relaxed);

    let mut page_table = PageTable::<CountingMeta, TestAllocator>::new(TestAllocator).unwrap();
    page_table
        .map_region(
            VirtAddr::from_usize(0x20_0000),
            |vaddr| PhysAddr::from_usize(vaddr.as_usize() + 0x20_0000),
            2 * CountingMeta::PAGE_SIZE,
            MappingFlags::READ | MappingFlags::WRITE,
            false,
        )
        .unwrap();

    assert_eq!(ADDRESS_FLUSHES.load(Ordering::Relaxed), 2);
    assert_eq!(FULL_FLUSHES.load(Ordering::Relaxed), 0);

    FULL_FLUSHES.store(0, Ordering::Relaxed);
    ADDRESS_FLUSHES.store(0, Ordering::Relaxed);

    let mut page_table = PageTable::<CountingMeta, TestAllocator>::new(TestAllocator).unwrap();
    page_table
        .map_region(
            VirtAddr::from_usize(0x40_0000),
            |vaddr| PhysAddr::from_usize(vaddr.as_usize() + 0x20_0000),
            128 * CountingMeta::PAGE_SIZE,
            MappingFlags::READ | MappingFlags::WRITE,
            false,
        )
        .unwrap();

    assert_eq!(ADDRESS_FLUSHES.load(Ordering::Relaxed), 0);
    assert_eq!(FULL_FLUSHES.load(Ordering::Relaxed), 1);
}
