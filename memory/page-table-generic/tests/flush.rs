//! TLB flush behavior for range operations.

#![cfg(not(target_os = "none"))]

use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use page_table_generic::*;

mod mocks;

use mocks::{Fram4k, MappingFlags, PteImpl};

static FULL_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static ADDRESS_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static FLUSH_TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
struct CountingMeta;

impl TableMeta for CountingMeta {
    type P = PteImpl;

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
    let _lock = FLUSH_TEST_LOCK.lock().unwrap();
    FULL_FLUSHES.store(0, Ordering::Relaxed);
    ADDRESS_FLUSHES.store(0, Ordering::Relaxed);

    let mut page_table = PageTable::<CountingMeta, Fram4k>::new(Fram4k).unwrap();
    page_table
        .map_region(
            VirtAddr::from_usize(0x20_0000),
            |vaddr| PhysAddr::from_usize(vaddr.as_usize() + 0x20_0000),
            2 * CountingMeta::PAGE_SIZE,
            (MappingFlags::READ | MappingFlags::WRITE).into(),
        )
        .unwrap();

    assert_eq!(ADDRESS_FLUSHES.load(Ordering::Relaxed), 2);
    assert_eq!(FULL_FLUSHES.load(Ordering::Relaxed), 0);

    FULL_FLUSHES.store(0, Ordering::Relaxed);
    ADDRESS_FLUSHES.store(0, Ordering::Relaxed);

    let mut page_table = PageTable::<CountingMeta, Fram4k>::new(Fram4k).unwrap();
    page_table
        .map_region(
            VirtAddr::from_usize(0x40_0000),
            |vaddr| PhysAddr::from_usize(vaddr.as_usize() + 0x20_0000),
            128 * CountingMeta::PAGE_SIZE,
            (MappingFlags::READ | MappingFlags::WRITE).into(),
        )
        .unwrap();

    assert_eq!(ADDRESS_FLUSHES.load(Ordering::Relaxed), 0);
    assert_eq!(FULL_FLUSHES.load(Ordering::Relaxed), 1);
}

static LEAF_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
static TABLE_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
static BREAK_BEFORE_MAKE_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
static REPLACEMENT_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct RoutedMeta;

impl TableMeta for RoutedMeta {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<VirtAddr>) {}

    fn flush_batch(_vaddrs: &[VirtAddr]) {
        TABLE_COMPLETIONS.fetch_add(1, Ordering::Relaxed);
    }

    fn flush_leaf_batch(vaddrs: &[VirtAddr]) {
        LEAF_COMPLETIONS.fetch_add(vaddrs.len(), Ordering::Relaxed);
    }

    fn flush_before_make(_vaddr: VirtAddr, _page_size: usize) -> page_table_generic::PagingResult {
        BREAK_BEFORE_MAKE_COMPLETIONS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn complete_replaced_leaf(_vaddr: VirtAddr) {
        REPLACEMENT_COMPLETIONS.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn leaf_updates_and_retained_paths_use_leaf_completion() {
    let _lock = FLUSH_TEST_LOCK.lock().unwrap();
    let mut page_table = PageTable::<RoutedMeta, Fram4k>::new(Fram4k).unwrap();
    let first = VirtAddr::from_usize(0x20_0000);
    let second = first + RoutedMeta::PAGE_SIZE;
    let original = PhysAddr::from_usize(0x40_0000);
    let replacement = PhysAddr::from_usize(0x50_0000);
    let read = MappingFlags::READ.into();

    page_table
        .map_page(first, original, RoutedMeta::PAGE_SIZE, read)
        .unwrap();
    page_table
        .map_page(
            second,
            original + RoutedMeta::PAGE_SIZE,
            RoutedMeta::PAGE_SIZE,
            read,
        )
        .unwrap();
    LEAF_COMPLETIONS.store(0, Ordering::Relaxed);
    TABLE_COMPLETIONS.store(0, Ordering::Relaxed);
    BREAK_BEFORE_MAKE_COMPLETIONS.store(0, Ordering::Relaxed);
    REPLACEMENT_COMPLETIONS.store(0, Ordering::Relaxed);

    page_table.protect_page(first, read).unwrap();
    assert_eq!(BREAK_BEFORE_MAKE_COMPLETIONS.load(Ordering::Relaxed), 0);
    page_table.remap_page(first, original, read).unwrap();
    assert_eq!(BREAK_BEFORE_MAKE_COMPLETIONS.load(Ordering::Relaxed), 0);
    assert_eq!(LEAF_COMPLETIONS.load(Ordering::Relaxed), 2);
    assert_eq!(TABLE_COMPLETIONS.load(Ordering::Relaxed), 0);

    page_table.remap_page(first, replacement, read).unwrap();
    assert_eq!(LEAF_COMPLETIONS.load(Ordering::Relaxed), 2);
    assert_eq!(BREAK_BEFORE_MAKE_COMPLETIONS.load(Ordering::Relaxed), 1);
    assert_eq!(REPLACEMENT_COMPLETIONS.load(Ordering::Relaxed), 1);

    page_table.unmap_page(first).unwrap();
    assert_eq!(LEAF_COMPLETIONS.load(Ordering::Relaxed), 3);
    assert_eq!(TABLE_COMPLETIONS.load(Ordering::Relaxed), 0);

    page_table.unmap_page(second).unwrap();
    assert_eq!(LEAF_COMPLETIONS.load(Ordering::Relaxed), 4);
    assert_eq!(TABLE_COMPLETIONS.load(Ordering::Relaxed), 0);
}

#[test]
fn memory_attribute_change_breaks_the_old_mapping_before_make() {
    let _lock = FLUSH_TEST_LOCK.lock().unwrap();
    let mut page_table = PageTable::<RoutedMeta, Fram4k>::new(Fram4k).unwrap();
    let address = VirtAddr::from_usize(0x20_0000);
    let physical = PhysAddr::from_usize(0x40_0000);
    page_table
        .map_page(
            address,
            physical,
            RoutedMeta::PAGE_SIZE,
            MappingFlags::READ.into(),
        )
        .unwrap();
    BREAK_BEFORE_MAKE_COMPLETIONS.store(0, Ordering::Relaxed);
    REPLACEMENT_COMPLETIONS.store(0, Ordering::Relaxed);
    LEAF_COMPLETIONS.store(0, Ordering::Relaxed);

    page_table
        .protect_page(
            address,
            (MappingFlags::READ | MappingFlags::UNCACHED).into(),
        )
        .unwrap();

    assert_eq!(BREAK_BEFORE_MAKE_COMPLETIONS.load(Ordering::Relaxed), 1);
    assert_eq!(REPLACEMENT_COMPLETIONS.load(Ordering::Relaxed), 1);
    assert_eq!(LEAF_COMPLETIONS.load(Ordering::Relaxed), 0);
    assert_eq!(page_table.query(address).unwrap().0, physical);
}

#[derive(Clone, Copy)]
struct FailedPreMakeMeta;

static FAILED_PRE_MAKE_RESTORES: AtomicUsize = AtomicUsize::new(0);

impl TableMeta for FailedPreMakeMeta {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<VirtAddr>) {}

    fn flush_before_make(_vaddr: VirtAddr, _page_size: usize) -> PagingResult {
        Err(PagingError::BreakBeforeMakeShootdownFailed)
    }

    fn publish_new_mapping(_vaddr: VirtAddr) {
        FAILED_PRE_MAKE_RESTORES.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn failed_pre_make_sync_republishes_the_old_mapping() {
    let mut page_table = PageTable::<FailedPreMakeMeta, Fram4k>::new(Fram4k).unwrap();
    let address = VirtAddr::from_usize(0x20_0000);
    let original = PhysAddr::from_usize(0x40_0000);
    let read = MappingFlags::READ.into();
    page_table
        .map_page(address, original, FailedPreMakeMeta::PAGE_SIZE, read)
        .unwrap();
    let original_mapping = page_table.query(address).unwrap();
    FAILED_PRE_MAKE_RESTORES.store(0, Ordering::Relaxed);

    assert!(matches!(
        page_table.remap_page(address, PhysAddr::from_usize(0x50_0000), read),
        Err(PagingError::BreakBeforeMakeShootdownFailed)
    ));
    assert_eq!(page_table.query(address).unwrap(), original_mapping);
    assert_eq!(FAILED_PRE_MAKE_RESTORES.load(Ordering::Relaxed), 1);

    assert!(matches!(
        page_table.protect_page(
            address,
            (MappingFlags::READ | MappingFlags::UNCACHED).into()
        ),
        Err(PagingError::BreakBeforeMakeShootdownFailed)
    ));
    assert_eq!(page_table.query(address).unwrap(), original_mapping);
    assert_eq!(FAILED_PRE_MAKE_RESTORES.load(Ordering::Relaxed), 2);
}
