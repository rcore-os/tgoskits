#![cfg(feature = "stage2")]
#![cfg(not(target_os = "none"))]

pub mod mocks;

use ax_page_table::stage2::*;
use mocks::*;

#[derive(Debug, Clone, Copy)]
struct Sv39x4Meta;

impl TableMeta for Sv39x4Meta {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[11, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 2;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(_vaddr: Option<VirtAddr>) {}
}

#[derive(Debug, Clone, Copy)]
struct Sv48x4Meta;

impl TableMeta for Sv48x4Meta {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[11, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(_vaddr: Option<VirtAddr>) {}
}

fn map_one<T: TableMeta<P = PteImpl>, A: PageFrameProvider>(
    pt: &mut PageTable<T, A>,
    gpa: usize,
    hpa: usize,
) {
    pt.map(&MapConfig {
        vaddr: VirtAddr::from_usize(gpa),
        paddr: PhysAddr::from_usize(hpa),
        size: T::PAGE_SIZE,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();
}

fn assert_root_pages_do_not_alias<T: TableMeta<P = PteImpl>>(root_page_span: usize) {
    let mut pt = PageTable::<T, Fram4k>::new(Fram4k).unwrap();
    for root_page in 0..4 {
        let gpa = root_page * root_page_span;
        let hpa = 0x1000_0000 + root_page * 0x20_0000;
        map_one(&mut pt, gpa, hpa);
    }

    for root_page in 0..4 {
        let gpa = root_page * root_page_span;
        let expected_hpa = 0x1000_0000 + root_page * 0x20_0000;
        let translated = pt.translate_phys(VirtAddr::from_usize(gpa)).unwrap();
        assert_eq!(translated, PhysAddr::from_usize(expected_hpa));
    }
}

#[test]
fn sv39x4_maps_all_four_root_pages_without_aliasing() {
    assert_root_pages_do_not_alias::<Sv39x4Meta>(1 << 39);
}

#[test]
fn sv48x4_maps_all_four_root_pages_without_aliasing() {
    assert_root_pages_do_not_alias::<Sv48x4Meta>(1 << 48);
}

#[test]
fn sv39x4_rejects_gpa_outside_41_bits() {
    let mut pt = PageTable::<Sv39x4Meta, Fram4k>::new(Fram4k).unwrap();
    let err = pt
        .map(&MapConfig {
            vaddr: VirtAddr::from_usize(1 << 41),
            paddr: PhysAddr::from_usize(0x2000_0000),
            size: 0x1000,
            pte: PteImpl::kernel_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap_err();
    assert!(matches!(err, PagingError::AddressOverflow { .. }));
}

#[test]
fn sv48x4_rejects_gpa_outside_50_bits() {
    let mut pt = PageTable::<Sv48x4Meta, Fram4k>::new(Fram4k).unwrap();
    let err = pt
        .map(&MapConfig {
            vaddr: VirtAddr::from_usize(1 << 50),
            paddr: PhysAddr::from_usize(0x2000_0000),
            size: 0x1000,
            pte: PteImpl::kernel_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap_err();
    assert!(matches!(err, PagingError::AddressOverflow { .. }));
}

#[test]
fn sv48x4_drop_releases_tables_from_high_root_quadrant() {
    let allocator = TrackedFram4k::new();
    {
        let mut pt = PageTable::<Sv48x4Meta, TrackedFram4k>::new(allocator).unwrap();
        map_one(&mut pt, 3 * (1 << 48), 0x3000_0000);
        assert!(allocator.allocated_count() > 1);
    }
    assert!(!allocator.has_leaks());
}
