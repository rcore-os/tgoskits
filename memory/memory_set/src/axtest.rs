use alloc::boxed::Box;

use ax_memory_addr::{VirtAddr, va_range};
use axtest::prelude::*;

use crate::{MappingBackend, MappingError, MemoryArea, MemorySet};

const MAX_ADDR: usize = 0x8000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];

#[derive(Clone)]
struct MockBackend;

type MockMemorySet = MemorySet<MockBackend>;

impl MappingBackend for MockBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;

    fn map(&self, start: VirtAddr, size: usize, flags: MockFlags, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return false;
            }
            *entry = flags;
        }
        true
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = 0;
        }
        true
    }

    fn protect(
        &self,
        start: VirtAddr,
        size: usize,
        new_flags: MockFlags,
        pt: &mut MockPageTable,
    ) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = new_flags;
        }
        true
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(Self)
    }
}

fn page_table() -> Box<MockPageTable> {
    Box::new([0; MAX_ADDR])
}

fn area(start: usize, size: usize, flags: MockFlags) -> MemoryArea<MockBackend> {
    MemoryArea::new(start.into(), size, flags, MockBackend)
}

#[axtest]
fn memory_set_maps_overlaps_replaces_and_clears_ranges() {
    let mut set = MockMemorySet::new();
    let mut pt = page_table();
    ax_assert!(set.is_empty());

    set.map(area(0, 0x1000, 1), &mut pt, false).unwrap();
    set.map(area(0x2000, 0x1000, 2), &mut pt, false).unwrap();
    ax_assert_eq!(set.len(), 2);
    ax_assert_eq!(set.find(0x100.into()).unwrap().flags(), 1);
    ax_assert!(set.find(0x1800.into()).is_none());
    ax_assert!(set.overlaps(va_range!(0x800..0x1800)));
    ax_assert!(!set.overlaps(va_range!(0x1000..0x2000)));

    ax_assert_eq!(
        set.map(area(0x800, 0x1000, 3), &mut pt, false),
        Err(MappingError::AlreadyExists)
    );
    set.map(area(0x800, 0x1000, 3), &mut pt, true).unwrap();
    ax_assert_eq!(set.find(0x900.into()).unwrap().flags(), 3);
    ax_assert_eq!(pt[0x900], 3);

    set.clear(&mut pt).unwrap();
    ax_assert!(set.is_empty());
    ax_assert!(pt.iter().all(|entry| *entry == 0));
}

#[axtest]
fn memory_set_unmap_splits_and_shrinks_boundary_areas() {
    let mut set = MockMemorySet::new();
    let mut pt = page_table();
    set.map(area(0, 0x3000, 1), &mut pt, false).unwrap();

    set.unmap(0x800.into(), 0x1000, &mut pt).unwrap();
    ax_assert_eq!(set.len(), 2);
    let first = set.find(0x100.into()).unwrap();
    ax_assert_eq!(first.start(), VirtAddr::from(0));
    ax_assert_eq!(first.end(), VirtAddr::from(0x800));
    let second = set.find(0x2000.into()).unwrap();
    ax_assert_eq!(second.start(), VirtAddr::from(0x1800));
    ax_assert_eq!(second.end(), VirtAddr::from(0x3000));
    ax_assert!(pt[0..0x800].iter().all(|entry| *entry == 1));
    ax_assert!(pt[0x800..0x1800].iter().all(|entry| *entry == 0));

    set.unmap(0x1800.into(), 0x800, &mut pt).unwrap();
    ax_assert_eq!(set.len(), 2);
    ax_assert!(set.find(0x1800.into()).is_none());
    ax_assert!(set.find(0x2800.into()).is_some());

    set.unmap(0.into(), MAX_ADDR, &mut pt).unwrap();
    ax_assert!(set.is_empty());
}

#[axtest]
fn memory_set_protect_splits_and_updates_reported_flags() {
    let mut set = MockMemorySet::new();
    let mut pt = page_table();
    set.map(
        MemoryArea::new_with_reported_flags(0.into(), 0x4000, 0x7, 0xf0, MockBackend),
        &mut pt,
        false,
    )
    .unwrap();

    set.protect_with_reported_flags(
        0x1000.into(),
        0x2000,
        |flags, reported| Some((flags & !0x2, reported | 0x1)),
        &mut pt,
    )
    .unwrap();
    ax_assert_eq!(set.len(), 3);

    let left = set.find(0x800.into()).unwrap();
    ax_assert_eq!(left.flags(), 0x7);
    ax_assert_eq!(left.reported_flags(), 0xf0);
    let middle = set.find(0x1800.into()).unwrap();
    ax_assert_eq!(middle.flags(), 0x5);
    ax_assert_eq!(middle.reported_flags(), 0xf1);
    let right = set.find(0x3800.into()).unwrap();
    ax_assert_eq!(right.flags(), 0x7);
    ax_assert_eq!(right.reported_flags(), 0xf0);
    ax_assert_eq!(pt[0x1800], 0x5);

    set.protect(0x1800.into(), 0x800, |_| None, &mut pt)
        .unwrap();
    ax_assert_eq!(set.len(), 3);
}

#[axtest]
fn memory_set_find_free_extend_and_metadata_operations_hold() {
    let mut set = MockMemorySet::new();
    let mut pt = page_table();
    set.map(area(0, 0x1000, 1), &mut pt, false).unwrap();
    set.map(area(0x3000, 0x1000, 2), &mut pt, false).unwrap();

    ax_assert_eq!(
        set.find_free_area(0.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000),
        Some(0x1000.into())
    );
    ax_assert_eq!(
        set.find_free_area(0x1800.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000),
        Some(0x2000.into())
    );
    ax_assert_eq!(
        set.find_free_area(0.into(), 0x1800, va_range!(0..MAX_ADDR), 0x1000),
        None
    );

    set.extend_area(0x100.into(), 0x1000, &mut pt).unwrap();
    ax_assert_eq!(set.find(0x1800.into()).unwrap().flags(), 1);
    ax_assert_eq!(
        set.extend_area(0x100.into(), 0x2000, &mut pt),
        Err(MappingError::AlreadyExists)
    );
    ax_assert_eq!(
        set.extend_area(0x6000.into(), 0x1000, &mut pt),
        Err(MappingError::InvalidParam)
    );

    set.unmap_metadata(0x800.into(), 0x800).unwrap();
    ax_assert_eq!(set.len(), 3);
    ax_assert_eq!(pt[0x900], 1);

    set.replace_area_metadata(MemoryArea::new_with_reported_flags(
        0x1000.into(),
        0x800,
        9,
        0x90,
        MockBackend,
    ))
    .unwrap();
    let replaced = set.find(0x1200.into()).unwrap();
    ax_assert_eq!(replaced.flags(), 9);
    ax_assert_eq!(replaced.reported_flags(), 0x90);
}
