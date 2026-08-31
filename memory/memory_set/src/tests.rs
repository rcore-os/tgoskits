use ax_memory_addr::{MemoryAddr, VirtAddr, va_range};

use crate::{MappingBackend, MappingError, MemoryArea, MemorySet};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];

#[derive(Clone)]
struct MockBackend;

#[derive(Clone)]
struct FailUnmapBackend;

#[derive(Clone)]
struct FailSecondUnmapBackend;

#[derive(Clone)]
struct FailSecondProtectBackend;

type MockMemorySet = MemorySet<MockBackend>;

impl MappingBackend for MockBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type MutationContext = ();
    type PageTable = MockPageTable;

    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MockFlags,
        _context: &mut (),
        pt: &mut MockPageTable,
    ) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return false;
            }
            *entry = flags;
        }
        true
    }

    fn unmap(
        &self,
        start: VirtAddr,
        size: usize,
        _context: &mut (),
        pt: &mut MockPageTable,
    ) -> bool {
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
        _context: &mut (),
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
        Some(self.clone())
    }

    fn shrink_left(&mut self, _shrink_size: usize) {}

    fn shrink_right(&mut self, _shrink_size: usize) {}
}

impl MappingBackend for FailUnmapBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type MutationContext = ();
    type PageTable = MockPageTable;

    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MockFlags,
        _context: &mut (),
        pt: &mut MockPageTable,
    ) -> bool {
        pt.iter_mut()
            .skip(start.as_usize())
            .take(size)
            .for_each(|entry| *entry = flags);
        true
    }

    fn unmap(
        &self,
        _start: VirtAddr,
        _size: usize,
        _context: &mut (),
        _pt: &mut MockPageTable,
    ) -> bool {
        false
    }

    fn validate_unmap(&self, _start: VirtAddr, _size: usize, _pt: &MockPageTable) -> bool {
        false
    }

    fn protect(
        &self,
        _start: VirtAddr,
        _size: usize,
        _new_flags: MockFlags,
        _context: &mut (),
        _pt: &mut MockPageTable,
    ) -> bool {
        false
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(Self)
    }
}

impl MappingBackend for FailSecondProtectBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type MutationContext = usize;
    type PageTable = MockPageTable;

    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MockFlags,
        _context: &mut usize,
        pt: &mut MockPageTable,
    ) -> bool {
        pt.iter_mut()
            .skip(start.as_usize())
            .take(size)
            .for_each(|entry| *entry = flags);
        true
    }

    fn unmap(
        &self,
        _start: VirtAddr,
        _size: usize,
        _context: &mut usize,
        _pt: &mut MockPageTable,
    ) -> bool {
        true
    }

    fn protect(
        &self,
        start: VirtAddr,
        size: usize,
        new_flags: MockFlags,
        attempts: &mut usize,
        pt: &mut MockPageTable,
    ) -> bool {
        *attempts += 1;
        if *attempts == 2 {
            return false;
        }
        pt.iter_mut()
            .skip(start.as_usize())
            .take(size)
            .for_each(|entry| *entry = new_flags);
        true
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(Self)
    }
}

impl MappingBackend for FailSecondUnmapBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type MutationContext = usize;
    type PageTable = MockPageTable;

    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MockFlags,
        _attempts: &mut usize,
        pt: &mut MockPageTable,
    ) -> bool {
        pt.iter_mut()
            .skip(start.as_usize())
            .take(size)
            .for_each(|entry| *entry = flags);
        true
    }

    fn unmap(
        &self,
        start: VirtAddr,
        size: usize,
        attempts: &mut usize,
        pt: &mut MockPageTable,
    ) -> bool {
        *attempts += 1;
        if *attempts == 2 {
            return false;
        }
        pt.iter_mut()
            .skip(start.as_usize())
            .take(size)
            .for_each(|entry| *entry = 0);
        true
    }

    fn protect(
        &self,
        _start: VirtAddr,
        _size: usize,
        _new_flags: MockFlags,
        _attempts: &mut usize,
        _pt: &mut MockPageTable,
    ) -> bool {
        true
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        Some(Self)
    }
}

macro_rules! assert_ok {
    ($expr:expr) => {
        assert!(($expr).is_ok())
    };
}

macro_rules! assert_err {
    ($expr:expr) => {
        assert!(($expr).is_err())
    };
    ($expr:expr, $err:ident) => {
        assert_eq!(($expr).err(), Some(MappingError::$err))
    };
}

fn dump_memory_set(set: &MockMemorySet) {
    use std::sync::Mutex;
    static DUMP_LOCK: Mutex<()> = Mutex::new(());

    let _lock = DUMP_LOCK.lock().unwrap();
    println!("Number of areas: {}", set.len());
    for area in set.iter() {
        println!("{:?}", area);
    }
}

#[test]
fn test_map_unmap() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut (),
            &mut pt,
            false,
        ));
    }
    // Map [0x1000, 0x2000), [0x3000, 0x4000), [0x5000, 0x6000), ...
    for start in (0x1000..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 2, MockBackend),
            &mut (),
            &mut pt,
            false,
        ));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 16);
    for &e in &pt[0..MAX_ADDR] {
        assert!(e == 1 || e == 2);
    }

    // Found [0x4000, 0x5000), flags = 1.
    let area = set.find(0x4100.into()).unwrap();
    assert_eq!(area.start(), 0x4000.into());
    assert_eq!(area.end(), 0x5000.into());
    assert_eq!(area.flags(), 1);
    assert_eq!(pt[0x4200], 1);

    // The area [0x4000, 0x8000) is already mapped, map returns an error.
    assert_err!(
        set.map(
            MemoryArea::new(0x4000.into(), 0x4000, 3, MockBackend),
            &mut (),
            &mut pt,
            false
        ),
        AlreadyExists
    );
    // Unmap overlapped areas before adding the new mapping [0x4000, 0x8000).
    assert_ok!(set.map(
        MemoryArea::new(0x4000.into(), 0x4000, 3, MockBackend),
        &mut (),
        &mut pt,
        true
    ));
    dump_memory_set(&set);
    assert_eq!(set.len(), 13);

    // Found [0x4000, 0x8000), flags = 3.
    let area = set.find(0x4100.into()).unwrap();
    assert_eq!(area.start(), 0x4000.into());
    assert_eq!(area.end(), 0x8000.into());
    assert_eq!(area.flags(), 3);
    for &e in &pt[0x4000..0x8000] {
        assert_eq!(e, 3);
    }

    // Unmap areas in the middle.
    assert_ok!(set.unmap(0x4000.into(), 0x8000, &mut (), &mut pt));
    assert_eq!(set.len(), 8);
    // Unmap the remaining areas, including the unmapped ranges.
    assert_ok!(set.unmap(0.into(), MAX_ADDR * 2, &mut (), &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn test_unmap_split() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut (),
            &mut pt,
            false,
        ));
    }
    assert_eq!(set.len(), 8);

    // Unmap [0xc00, 0x2400), [0x2c00, 0x4400), [0x4c00, 0x6400), ...
    // The areas are shrinked at the left and right boundaries.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.unmap((start + 0xc00).into(), 0x1800, &mut (), &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 8);

    for area in set.iter() {
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0xc00);
        } else {
            assert_eq!(area.start().align_offset_4k(), 0x400);
            assert_eq!(area.end().align_offset_4k(), 0xc00);
            assert_eq!(area.size(), 0x800);
        }
        for &e in &pt[area.start().as_usize()..area.end().as_usize()] {
            assert_eq!(e, 1);
        }
    }

    // Unmap [0x800, 0x900), [0x2800, 0x2900), [0x4800, 0x4900), ...
    // The areas are split into two areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.unmap((start + 0x800).into(), 0x100, &mut (), &mut pt));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 16);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if off == 0 {
            assert_eq!(area.size(), 0x800);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x400);
        } else if off == 0x900 {
            assert_eq!(area.size(), 0x300);
        } else {
            unreachable!();
        }
        for &e in &pt[area.start().as_usize()..area.end().as_usize()] {
            assert_eq!(e, 1);
        }
    }
    let mut iter = set.iter();
    while let Some(area) = iter.next() {
        if let Some(next) = iter.next() {
            for &e in &pt[area.end().as_usize()..next.start().as_usize()] {
                assert_eq!(e, 0);
            }
        }
    }
    drop(iter);

    // Unmap all areas.
    assert_ok!(set.unmap(0.into(), MAX_ADDR, &mut (), &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn failed_boundary_unmap_preserves_complete_area_metadata() {
    let mut middle = MemorySet::<FailUnmapBackend>::new();
    let mut middle_pt = [0; MAX_ADDR];
    middle
        .map(
            MemoryArea::new(0x1000.into(), 0x4000, 1, FailUnmapBackend),
            &mut (),
            &mut middle_pt,
            false,
        )
        .unwrap();
    assert_eq!(
        middle.unmap(0x2000.into(), 0x1000, &mut (), &mut middle_pt),
        Err(MappingError::BadState)
    );
    let area = middle.find(0x4000.into()).unwrap();
    assert_eq!(area.start(), VirtAddr::from(0x1000));
    assert_eq!(area.end(), VirtAddr::from(0x5000));
    assert_eq!(middle.len(), 1);

    let mut left = MemorySet::<FailUnmapBackend>::new();
    let mut left_pt = [0; MAX_ADDR];
    left.map(
        MemoryArea::new(0x3000.into(), 0x2000, 1, FailUnmapBackend),
        &mut (),
        &mut left_pt,
        false,
    )
    .unwrap();
    assert_eq!(
        left.unmap(0x2000.into(), 0x2000, &mut (), &mut left_pt),
        Err(MappingError::BadState)
    );
    let area = left.find(0x4000.into()).unwrap();
    assert_eq!(area.start(), VirtAddr::from(0x3000));
    assert_eq!(area.end(), VirtAddr::from(0x5000));
    assert_eq!(left.len(), 1);
}

#[test]
fn failed_multi_area_unmap_keeps_all_backend_owners_for_retry() {
    let mut set = MemorySet::<FailSecondUnmapBackend>::new();
    let mut pt = [0; MAX_ADDR];
    let mut attempts = 0;
    for start in [0x1000, 0x3000] {
        set.map(
            MemoryArea::new(start.into(), 0x1000, 0x7, FailSecondUnmapBackend),
            &mut attempts,
            &mut pt,
            false,
        )
        .unwrap();
    }

    assert_eq!(
        set.unmap(0x1000.into(), 0x3000, &mut attempts, &mut pt),
        Err(MappingError::BadState)
    );
    assert_eq!(set.len(), 2, "a failed transaction must retain every owner");
    assert!(set.find(0x1000.into()).is_some());
    assert!(set.find(0x3000.into()).is_some());
    assert!(pt[0x1000..0x2000].iter().all(|entry| *entry == 0));
    assert!(pt[0x3000..0x4000].iter().all(|entry| *entry == 0x7));

    assert_ok!(set.unmap(0x1000.into(), 0x3000, &mut attempts, &mut pt));
    assert!(set.is_empty());
}

#[test]
fn failed_multi_area_protect_rolls_back_page_table_and_metadata() {
    let mut set = MemorySet::<FailSecondProtectBackend>::new();
    let mut pt = [0; MAX_ADDR];
    let mut attempts = 0;
    for start in [0x1000, 0x3000] {
        set.map(
            MemoryArea::new(start.into(), 0x1000, 0x7, FailSecondProtectBackend),
            &mut attempts,
            &mut pt,
            false,
        )
        .unwrap();
    }

    assert_eq!(
        set.protect(0x1000.into(), 0x3000, |_| Some(0x1), &mut attempts, &mut pt,),
        Err(MappingError::BadState)
    );
    assert_eq!(set.find(0x1000.into()).unwrap().flags(), 0x7);
    assert_eq!(set.find(0x3000.into()).unwrap().flags(), 0x7);
    assert!(pt[0x1000..0x2000].iter().all(|entry| *entry == 0x7));
    assert!(pt[0x3000..0x4000].iter().all(|entry| *entry == 0x7));
}

#[test]
fn test_protect() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];
    let update_flags = |new_flags: MockFlags| {
        move |old_flags: MockFlags| -> Option<MockFlags> {
            if (old_flags & 0x7) == (new_flags & 0x7) {
                return None;
            }
            let flags = (new_flags & 0x7) | (old_flags & !0x7);
            Some(flags)
        }
    };

    // Map [0, 0x1000), [0x2000, 0x3000), [0x4000, 0x5000), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 0x7, MockBackend),
            &mut (),
            &mut pt,
            false,
        ));
    }
    assert_eq!(set.len(), 8);

    // Protect [0xc00, 0x2400), [0x2c00, 0x4400), [0x4c00, 0x6400), ...
    // The areas are split into two areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect(
            (start + 0xc00).into(),
            0x1800,
            update_flags(0x1),
            &mut (),
            &mut pt
        ));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 23);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0xc00);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x800);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0xc00 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        }
    }

    // Protect [0x800, 0x900), [0x2800, 0x2900), [0x4800, 0x4900), ...
    // The areas are split into three areas.
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect(
            (start + 0x800).into(),
            0x100,
            update_flags(0x13),
            &mut (),
            &mut pt
        ));
    }
    dump_memory_set(&set);
    assert_eq!(set.len(), 39);

    for area in set.iter() {
        let off = area.start().align_offset_4k();
        if area.start().as_usize() == 0 {
            assert_eq!(area.size(), 0x800);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        } else if off == 0x400 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0x800 {
            assert_eq!(area.size(), 0x100);
            assert_eq!(area.flags(), 0x3);
        } else if off == 0x900 {
            assert_eq!(area.size(), 0x300);
            assert_eq!(area.flags(), 0x7);
        } else if off == 0xc00 {
            assert_eq!(area.size(), 0x400);
            assert_eq!(area.flags(), 0x1);
        }
    }

    // Test skip [0x880, 0x900), [0x2880, 0x2900), [0x4880, 0x4900), ...
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.protect(
            (start + 0x880).into(),
            0x80,
            update_flags(0x3),
            &mut (),
            &mut pt
        ));
    }
    assert_eq!(set.len(), 39);

    // Unmap all areas.
    assert_ok!(set.unmap(0.into(), MAX_ADDR, &mut (), &mut pt));
    assert_eq!(set.len(), 0);
    for &e in &pt[0..MAX_ADDR] {
        assert_eq!(e, 0);
    }
}

#[test]
fn test_find_free_area() {
    let mut set = MockMemorySet::new();
    let mut pt = [0; MAX_ADDR];

    // Map [0, 0x1000), [0x2000, 0x3000), ..., [0xe000, 0xf000)
    for start in (0..MAX_ADDR).step_by(0x2000) {
        assert_ok!(set.map(
            MemoryArea::new(start.into(), 0x1000, 1, MockBackend),
            &mut (),
            &mut pt,
            false,
        ));
    }

    let addr = set.find_free_area(0.into(), 0x1000, va_range!(0..MAX_ADDR), 1);
    assert_eq!(addr, Some(0x1000.into()));

    let addr = set.find_free_area(0x800.into(), 0x800, va_range!(0..MAX_ADDR), 0x800);
    assert_eq!(addr, Some(0x1000.into()));

    let addr = set.find_free_area(0x1800.into(), 0x800, va_range!(0..MAX_ADDR), 0x800);
    assert_eq!(addr, Some(0x1800.into()));

    let addr = set.find_free_area(0x1800.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0x3000.into()));

    let addr = set.find_free_area(0x2000.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0x3000.into()));

    let addr = set.find_free_area(0xf000.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, Some(0xf000.into()));

    let addr = set.find_free_area(0xf001.into(), 0x1000, va_range!(0..MAX_ADDR), 0x1000);
    assert_eq!(addr, None);
}
