//! Tests for lowmem (DMA32) page allocation via GlobalAllocator.

extern crate buddy_slab_allocator;

mod common;

use buddy_slab_allocator::GlobalAllocator;
use common::{GlobalTestContext, HostRegion, init_global, virt_to_phys};

const PAGE_SIZE: usize = 0x1000;
const TEST_HEAP_SIZE: usize = 16 * 1024 * 1024;

fn init_allocator(
    allocator: &GlobalAllocator<PAGE_SIZE>,
    region: &mut HostRegion,
) -> GlobalTestContext {
    init_global(allocator, region, 1)
}

#[test]
fn test_lowmem_basic() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_allocator(&allocator, &mut region);
    let section = allocator.managed_section(0).unwrap();
    let managed_start = section.start;
    let managed_end = managed_start + section.size;

    let addr1 = allocator.alloc_pages_lowmem(1, PAGE_SIZE).unwrap();
    let addr2 = allocator.alloc_pages_lowmem(4, PAGE_SIZE).unwrap();

    assert!(addr1 >= managed_start && addr1 < managed_end);
    assert!(addr2 >= managed_start && addr2 < managed_end);
    assert_eq!(addr1 % PAGE_SIZE, 0);
    assert_eq!(addr2 % PAGE_SIZE, 0);

    allocator.dealloc_pages(addr1, 1);
    allocator.dealloc_pages(addr2, 4);
}

#[test]
fn test_lowmem_aligned() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 2);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_allocator(&allocator, &mut region);

    let addr = allocator.alloc_pages_lowmem(1, 2 * PAGE_SIZE).unwrap();
    assert_eq!(addr % (2 * PAGE_SIZE), 0);
    allocator.dealloc_pages(addr, 1);
}

#[test]
fn test_lowmem_vs_normal() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_allocator(&allocator, &mut region);

    let addr_low = allocator.alloc_pages_lowmem(1, PAGE_SIZE).unwrap();
    let addr_normal = allocator.alloc_pages(1, PAGE_SIZE).unwrap();

    assert!(addr_low >= allocator.managed_section(0).unwrap().start);
    assert!(addr_normal >= allocator.managed_section(0).unwrap().start);

    allocator.dealloc_pages(addr_low, 1);
    allocator.dealloc_pages(addr_normal, 1);
}

#[test]
fn test_lowmem_stress() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_allocator(&allocator, &mut region);

    let mut addrs = Vec::new();
    for _ in 0..32 {
        let addr = allocator.alloc_pages_lowmem(1, PAGE_SIZE).unwrap();
        addrs.push(addr);
    }
    for addr in addrs {
        allocator.dealloc_pages(addr, 1);
    }
}

#[test]
fn global_add_region_unaligned_lowmem_alignment() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;

    let mut first = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE);
    let mut second = HostRegion::new(8 * ALIGN_2M + 0x1234 + PAGE_SIZE, ALIGN_2M);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_allocator(&allocator, &mut first);

    let second_len = second.len();
    let second_slice = unsafe { second.subslice(0x1234, second_len - 0x1234 - PAGE_SIZE / 3) };
    unsafe { allocator.add_region(second_slice).unwrap() };

    let addr = allocator.alloc_pages_lowmem(1, ALIGN_2M).unwrap();
    assert_eq!(addr % ALIGN_2M, 0);
    assert!(virt_to_phys(addr) + PAGE_SIZE <= 0x1000_0000);
    allocator.dealloc_pages(addr, 1);
}
