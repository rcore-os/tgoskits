mod mocks;

use core::fmt;
use std::alloc::{self, Layout};

use mocks::{Fram4k, MappingFlags, PteImpl};
use page_table_generic::{
    FrameAllocator, PageTable, PageTableEntry, PhysAddr, TableMeta, VirtAddr, WalkConfig,
};

const PAGE_SIZE_16K: usize = 0x4000;
const BLOCK_SIZE_32M: usize = PAGE_SIZE_16K << 11;
const BLOCK_SIZE_2M: usize = OpaqueMeta::PAGE_SIZE << OpaqueMeta::LEVEL_BITS[0];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpaqueConfig {
    domain: u8,
    present: bool,
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
        let present = if config.present { Self::PRESENT } else { 0 };
        Self(
            (paddr.as_usize() as u64 & Self::PADDR_MASK)
                | present
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
            present: self.present(),
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
fn maps_and_protects_with_an_opaque_pte_config() {
    let mut page_table = PageTable::<OpaqueMeta, Fram4k>::new(Fram4k).unwrap();
    let vaddr = VirtAddr::from_usize(0x4000);
    let paddr = PhysAddr::from_usize(0x8000);

    page_table
        .map_page(
            vaddr,
            paddr,
            0x1000,
            OpaqueConfig {
                domain: 0x2a,
                present: true,
            },
        )
        .unwrap();
    assert_eq!(page_table.query(vaddr).unwrap().1.domain, 0x2a);

    page_table
        .protect_page(
            vaddr,
            OpaqueConfig {
                domain: 0x7f,
                present: true,
            },
        )
        .unwrap();
    assert_eq!(page_table.query(vaddr).unwrap().1.domain, 0x7f);
}

#[test]
fn occupied_walk_retains_non_present_leaf_identity() {
    let mut page_table = PageTable::<OpaqueMeta, Fram4k>::new(Fram4k).unwrap();
    let vaddr = VirtAddr::from_usize(0x4000);
    let paddr = PhysAddr::from_usize(0x8000);

    page_table
        .map_page(
            vaddr,
            paddr,
            OpaqueMeta::PAGE_SIZE,
            OpaqueConfig {
                domain: 0x2a,
                present: false,
            },
        )
        .unwrap();

    assert!(page_table.query(vaddr).is_err());
    let occupied = page_table.walk_occupied().collect::<Vec<_>>();
    assert_eq!(occupied.len(), 1);
    assert_eq!(occupied[0].vaddr, vaddr);
    assert_eq!(occupied[0].level, 1);
    assert_eq!(page_table.walk_valid().count(), 0);
    assert_eq!(
        page_table.mapping_size_for_level(occupied[0].level),
        Some(OpaqueMeta::PAGE_SIZE)
    );
}

#[test]
fn ranged_walk_descends_into_an_overlapping_parent_entry() {
    let mut page_table = PageTable::<OpaqueMeta, Fram4k>::new(Fram4k).unwrap();
    let vaddr = VirtAddr::from_usize(0x20_4000);
    let paddr = PhysAddr::from_usize(0x80_0000);

    page_table
        .map_page(
            vaddr,
            paddr,
            OpaqueMeta::PAGE_SIZE,
            OpaqueConfig {
                domain: 0x2a,
                present: false,
            },
        )
        .unwrap();

    let occupied = page_table
        .walk_all(WalkConfig {
            start_vaddr: vaddr,
            end_vaddr: vaddr + OpaqueMeta::PAGE_SIZE,
        })
        .filter(|entry| {
            !entry.pte.unused() && (entry.level == 1 || entry.pte.huge(entry.level > 1))
        })
        .collect::<Vec<_>>();

    assert_eq!(occupied.len(), 1);
    assert_eq!(occupied[0].vaddr, vaddr);
    assert_eq!(occupied[0].level, 1);

    let occupied = page_table
        .walk_occupied_range(vaddr, vaddr + OpaqueMeta::PAGE_SIZE)
        .collect::<Vec<_>>();
    assert_eq!(occupied.len(), 1);
    assert_eq!(occupied[0].vaddr, vaddr);
}

#[test]
fn partial_protect_splits_a_huge_leaf_without_changing_neighbors() {
    let mut page_table = PageTable::<OpaqueMeta, Fram4k>::new(Fram4k).unwrap();
    let vaddr = VirtAddr::from_usize(BLOCK_SIZE_2M);
    let paddr = PhysAddr::from_usize(BLOCK_SIZE_2M * 2);
    let original = OpaqueConfig {
        domain: 0x2a,
        present: true,
    };
    let protected = OpaqueConfig {
        domain: 0x7f,
        present: true,
    };

    page_table
        .map_linear_pages(vaddr, paddr, BLOCK_SIZE_2M, original, true)
        .unwrap();
    assert_eq!(page_table.query(vaddr).unwrap().2, BLOCK_SIZE_2M);

    page_table
        .protect_region(
            vaddr + OpaqueMeta::PAGE_SIZE,
            OpaqueMeta::PAGE_SIZE,
            protected,
        )
        .unwrap();

    assert_eq!(
        page_table.query(vaddr + OpaqueMeta::PAGE_SIZE).unwrap(),
        (
            paddr + OpaqueMeta::PAGE_SIZE,
            protected,
            OpaqueMeta::PAGE_SIZE,
        )
    );
    assert_eq!(page_table.query(vaddr).unwrap().1, original);
    assert_eq!(
        page_table
            .query(vaddr + OpaqueMeta::PAGE_SIZE * 2)
            .unwrap()
            .1,
        original
    );
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
fn map_linear_pages_selects_page_sizes_from_table_levels() {
    let mut page_table = PageTable::<T16kL4, Fram16k>::new(Fram16k).unwrap();
    let vaddr = VirtAddr::from_usize(BLOCK_SIZE_32M);
    let paddr = PhysAddr::from_usize(BLOCK_SIZE_32M);

    page_table
        .map_linear_pages(
            vaddr,
            paddr,
            BLOCK_SIZE_32M,
            MappingFlags::READ.into(),
            true,
        )
        .unwrap();

    let (mapped_paddr, _, page_size) = page_table.query(vaddr + PAGE_SIZE_16K).unwrap();
    assert_eq!(mapped_paddr, paddr + PAGE_SIZE_16K);
    assert_eq!(page_size, BLOCK_SIZE_32M);
}

#[test]
fn resolver_mapping_does_not_infer_physical_contiguity() {
    let mut page_table = PageTable::<OpaqueMeta, Fram4k>::new(Fram4k).unwrap();
    let vaddr = VirtAddr::from_usize(BLOCK_SIZE_2M);
    let paddr = PhysAddr::from_usize(BLOCK_SIZE_2M * 2);

    page_table
        .map_region(
            vaddr,
            |current| paddr + (current - vaddr),
            BLOCK_SIZE_2M,
            OpaqueConfig {
                domain: 0x2a,
                present: true,
            },
        )
        .unwrap();

    let (mapped_paddr, _, page_size) = page_table.query(vaddr + OpaqueMeta::PAGE_SIZE).unwrap();
    assert_eq!(mapped_paddr, paddr + OpaqueMeta::PAGE_SIZE);
    assert_eq!(page_size, OpaqueMeta::PAGE_SIZE);
}
