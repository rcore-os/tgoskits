//! TLB flush behavior for range operations.

#![cfg(not(target_os = "none"))]

use std::sync::atomic::{AtomicUsize, Ordering};

use page_table_generic::*;

mod mocks;

use mocks::{Fram4k, MappingFlags, PteImpl};

static FULL_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static ADDRESS_FLUSHES: AtomicUsize = AtomicUsize::new(0);

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
