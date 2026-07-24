//! Integration tests for the buddy-slab-allocator crate.

extern crate buddy_slab_allocator;
mod common;

use core::{alloc::Layout, ptr::NonNull};
use std::collections::BTreeSet;

use buddy_slab_allocator::{
    AllocError, BuddyAllocator, GlobalAllocator, ManagedSection, SizeClass, SlabAllocResult,
    SlabAllocator, SlabDeallocResult, slab::SlabPageHeader,
};
use common::{
    GlobalTestContext, HostRegion, count_free_pages, global_test_context,
    init_global as init_global_allocator, init_global_slice, set_current_cpu, set_physical_offset,
    virt_to_phys,
};

const PAGE_SIZE: usize = 0x1000;
const TEST_HEAP_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

fn buddy_region_size(heap_size: usize) -> usize {
    heap_size + BuddyAllocator::<PAGE_SIZE>::required_meta_size(heap_size) + PAGE_SIZE * 4
}

fn init_buddy(buddy: &mut BuddyAllocator<PAGE_SIZE>, region: &mut HostRegion) -> ManagedSection {
    unsafe { buddy.init(region.as_mut_slice()).unwrap() };
    buddy.section(0).unwrap()
}

fn init_buddy_with_heap_alignment(
    buddy: &mut BuddyAllocator<PAGE_SIZE>,
    region: &mut HostRegion,
    heap_align: usize,
) -> ManagedSection {
    for offset in (0..heap_align).step_by(PAGE_SIZE) {
        if region.len() <= offset {
            break;
        }
        let slice = unsafe { region.subslice(offset, region.len() - offset) };
        if unsafe { buddy.init(slice) }.is_ok() {
            let section = buddy.section(0).unwrap();
            if section.start.is_multiple_of(heap_align) {
                return section;
            }
        }
    }
    panic!("failed to find test region with heap alignment {heap_align:#x}");
}

fn primary_section(allocator: &GlobalAllocator<PAGE_SIZE>) -> ManagedSection {
    allocator.managed_section(0).unwrap()
}

fn irregular_region(size: usize, offset: usize, trim: usize, host_align: usize) -> HostRegion {
    HostRegion::new(size + offset + trim + PAGE_SIZE, host_align)
}

// ======================================================================
// Buddy allocator (standalone) tests
// ======================================================================

#[test]
fn buddy_basic_alloc_dealloc() {
    let mut region = HostRegion::new(buddy_region_size(TEST_HEAP_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let section = init_buddy(&mut buddy, &mut region);

    let addr1 = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr1 >= section.start && addr1 < section.start + section.size);
    assert_eq!(addr1 % PAGE_SIZE, 0);

    let addr4 = buddy.alloc_pages(4, PAGE_SIZE).unwrap();
    assert_eq!(addr4 % PAGE_SIZE, 0);

    let free_before = buddy.free_pages();
    buddy.dealloc_pages(addr1, 1);
    buddy.dealloc_pages(addr4, 4);
    assert!(buddy.free_pages() > free_before);
}

#[test]
fn buddy_alignment() {
    // Heap must be aligned to the highest alignment we test (PAGE_SIZE * 4)
    let mut region = HostRegion::new(
        buddy_region_size(TEST_HEAP_SIZE) + PAGE_SIZE * 4,
        PAGE_SIZE * 4,
    );
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy_with_heap_alignment(&mut buddy, &mut region, PAGE_SIZE * 4);

    let addr2 = buddy.alloc_pages(1, PAGE_SIZE * 2).unwrap();
    assert_eq!(addr2 % (PAGE_SIZE * 2), 0);

    let addr4 = buddy.alloc_pages(1, PAGE_SIZE * 4).unwrap();
    assert_eq!(addr4 % (PAGE_SIZE * 4), 0);

    buddy.dealloc_pages(addr2, 1);
    buddy.dealloc_pages(addr4, 1);
}

#[test]
fn buddy_add_region_unaligned_start_preserves_4k_alignment() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;
    let mut first = HostRegion::new(buddy_region_size(32 * PAGE_SIZE) + ALIGN_2M, ALIGN_2M);
    let mut second = irregular_region(
        buddy_region_size(64 * PAGE_SIZE),
        0x1234,
        PAGE_SIZE / 3,
        2 * 1024 * 1024,
    );
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _first_section = init_buddy_with_heap_alignment(&mut buddy, &mut first, ALIGN_2M);

    let second_len = second.len();
    let second_slice = unsafe { second.subslice(0x1234, second_len - 0x1234 - PAGE_SIZE / 3) };

    unsafe { buddy.add_region(second_slice).unwrap() };

    let addr = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert_eq!(addr % PAGE_SIZE, 0);
    buddy.dealloc_pages(addr, 1);
}

#[test]
fn buddy_add_region_unaligned_start_preserves_2m_alignment() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;

    let mut first = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut second = irregular_region(
        buddy_region_size(4 * ALIGN_2M),
        PAGE_SIZE / 2,
        PAGE_SIZE / 3,
        ALIGN_2M,
    );
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _first_section = init_buddy(&mut buddy, &mut first);

    let second_len = second.len();
    let second_slice =
        unsafe { second.subslice(PAGE_SIZE / 2, second_len - PAGE_SIZE / 2 - PAGE_SIZE / 3) };

    unsafe { buddy.add_region(second_slice).unwrap() };

    let addr = buddy.alloc_pages(1, ALIGN_2M).unwrap();
    assert_eq!(addr % ALIGN_2M, 0);
    buddy.dealloc_pages(addr, 1);
}

#[test]
fn buddy_aligned_alloc_dealloc_uses_recorded_order() {
    let heap_size = 64 * PAGE_SIZE;
    let mut region = HostRegion::new(
        buddy_region_size(heap_size) + PAGE_SIZE * 16,
        PAGE_SIZE * 16,
    );
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy_with_heap_alignment(&mut buddy, &mut region, PAGE_SIZE * 16);

    let free_before = buddy.free_pages();
    let addr = buddy.alloc_pages(4, PAGE_SIZE * 16).unwrap();
    buddy.dealloc_pages(addr, 4);
    assert_eq!(buddy.free_pages(), free_before);
}

#[test]
fn buddy_exhaust_and_recover() {
    let heap_size = 64 * PAGE_SIZE; // Small heap
    let mut region = HostRegion::new(buddy_region_size(heap_size), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    let mut addrs = Vec::new();
    while let Ok(addr) = buddy.alloc_pages(1, PAGE_SIZE) {
        addrs.push(addr);
    }
    assert_eq!(buddy.free_pages(), 0);

    // Free half
    for addr in addrs.drain(..addrs.len() / 2) {
        buddy.dealloc_pages(addr, 1);
    }
    assert!(buddy.free_pages() > 0);

    // Allocate again
    let addr = buddy.alloc_pages(1, PAGE_SIZE);
    assert!(addr.is_ok());

    // Cleanup
    if let Ok(a) = addr {
        buddy.dealloc_pages(a, 1);
    }
    for a in addrs {
        buddy.dealloc_pages(a, 1);
    }
}

#[test]
fn buddy_merge_coalescing() {
    let heap_size = 16 * PAGE_SIZE;
    let mut region = HostRegion::new(buddy_region_size(heap_size), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    let initial_free = buddy.free_pages();

    // Allocate two single pages
    let a = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    let b = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    buddy.dealloc_pages(a, 1);
    buddy.dealloc_pages(b, 1);

    // After freeing both, free_pages should return to initial
    assert_eq!(buddy.free_pages(), initial_free);
}

#[test]
fn buddy_fragmentation_blocks_high_order_then_recovers() {
    let heap_size = 32 * PAGE_SIZE;
    let mut region = HostRegion::new(buddy_region_size(heap_size), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let section = init_buddy(&mut buddy, &mut region);

    let mut addrs = Vec::new();
    while let Ok(addr) = buddy.alloc_pages(1, PAGE_SIZE) {
        addrs.push(addr);
    }
    assert_eq!(addrs.len(), section.total_pages);

    for &addr in addrs.iter().step_by(2) {
        buddy.dealloc_pages(addr, 1);
    }
    assert!(buddy.alloc_pages(2, PAGE_SIZE).is_err());

    for &addr in addrs.iter().skip(1).step_by(2) {
        buddy.dealloc_pages(addr, 1);
    }

    let addr = buddy.alloc_pages(8, PAGE_SIZE).unwrap();
    buddy.dealloc_pages(addr, 8);
    assert_eq!(buddy.free_pages(), section.total_pages);
}

#[test]
fn buddy_high_order_full_cycle_restores_free_pages() {
    let heap_size = 256 * PAGE_SIZE;
    let mut region = HostRegion::new(
        buddy_region_size(heap_size) + PAGE_SIZE * 16,
        PAGE_SIZE * 16,
    );
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy_with_heap_alignment(&mut buddy, &mut region, PAGE_SIZE * 16);

    let initial_free = buddy.free_pages();
    let requests = [
        (1usize, PAGE_SIZE),
        (2, 2 * PAGE_SIZE),
        (3, 4 * PAGE_SIZE),
        (8, 8 * PAGE_SIZE),
        (5, PAGE_SIZE),
        (16, 16 * PAGE_SIZE),
    ];
    let mut allocations = Vec::new();

    for (count, align) in requests {
        let addr = buddy.alloc_pages(count, align).unwrap();
        allocations.push((addr, count));
    }
    assert!(buddy.free_pages() < initial_free);

    for (addr, count) in allocations.into_iter().rev() {
        buddy.dealloc_pages(addr, count);
    }
    assert_eq!(buddy.free_pages(), initial_free);
}

#[test]
fn buddy_add_region_enables_second_section_allocation() {
    let mut first = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(64 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let first_section = init_buddy(&mut buddy, &mut first);

    while buddy.alloc_pages(1, PAGE_SIZE).is_ok() {}
    assert_eq!(buddy.free_pages(), 0);

    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };
    assert_eq!(buddy.section_count(), 2);
    let second_section = buddy.section(1).unwrap();

    let addr = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr >= second_section.start && addr < second_section.start + second_section.size);
    assert!(addr < first_section.start || addr >= first_section.start + first_section.size);
}

#[test]
fn buddy_add_region_overlap_rejected() {
    let mut region = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    let overlap = unsafe { region.subslice(1, region.len() - 1) };
    let err = unsafe { buddy.add_region(overlap) }.unwrap_err();
    assert_eq!(err, AllocError::MemoryOverlap);
}

#[test]
fn buddy_alloc_pages_first_fit_by_registration_order() {
    let mut first = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(64 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let first_section = init_buddy(&mut buddy, &mut first);
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };

    let addr = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr >= first_section.start && addr < first_section.start + first_section.size);
}

#[test]
fn buddy_lowmem_scans_across_sections() {
    let mut first = HostRegion::new(buddy_region_size(16 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _first_section = init_buddy(&mut buddy, &mut first);

    while buddy.alloc_pages_lowmem(1, PAGE_SIZE).is_ok() {}
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };
    let second_section = buddy.section(1).unwrap();

    let addr = buddy.alloc_pages_lowmem(1, PAGE_SIZE).unwrap();
    assert!(addr >= second_section.start && addr < second_section.start + second_section.size);
}

#[test]
fn buddy_dealloc_pages_finds_correct_section() {
    let mut first = HostRegion::new(buddy_region_size(16 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _first_section = init_buddy(&mut buddy, &mut first);
    while buddy.alloc_pages(1, PAGE_SIZE).is_ok() {}
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };
    let baseline = buddy.free_pages();
    let second_section = buddy.section(1).unwrap();

    let addr = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr >= second_section.start && addr < second_section.start + second_section.size);
    buddy.dealloc_pages(addr, 1);

    assert_eq!(buddy.free_pages(), baseline);
}

#[test]
fn buddy_total_and_free_pages_are_aggregated() {
    let mut first = HostRegion::new(buddy_region_size(16 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let first_section = init_buddy(&mut buddy, &mut first);
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };
    let second_section = buddy.section(1).unwrap();

    assert_eq!(
        buddy.total_pages(),
        first_section.total_pages + second_section.total_pages
    );
    assert_eq!(
        buddy.free_pages(),
        first_section.free_pages + second_section.free_pages
    );
}

#[test]
fn buddy_managed_bytes_matches_all_sections() {
    let mut first = HostRegion::new(buddy_region_size(16 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let first_section = init_buddy(&mut buddy, &mut first);
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };
    let second_section = buddy.section(1).unwrap();

    assert_eq!(
        buddy.managed_bytes(),
        first_section.size + second_section.size
    );
}

#[test]
fn buddy_allocated_bytes_changes_with_page_alloc_free() {
    let mut region = HostRegion::new(buddy_region_size(64 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    assert_eq!(buddy.allocated_bytes(), 0);

    let a = buddy.alloc_pages(1, PAGE_SIZE).unwrap();
    assert_eq!(buddy.allocated_bytes(), PAGE_SIZE);

    let b = buddy.alloc_pages(4, PAGE_SIZE).unwrap();
    assert_eq!(buddy.allocated_bytes(), 5 * PAGE_SIZE);

    buddy.dealloc_pages(a, 1);
    assert_eq!(buddy.allocated_bytes(), 4 * PAGE_SIZE);

    buddy.dealloc_pages(b, 4);
    assert_eq!(buddy.allocated_bytes(), 0);
}

#[test]
fn buddy_allocated_bytes_zero_when_all_free() {
    let mut region = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    assert_eq!(buddy.allocated_bytes(), 0);
}

#[test]
fn buddy_allocated_bytes_aggregates_across_sections() {
    let mut first = HostRegion::new(buddy_region_size(16 * PAGE_SIZE), PAGE_SIZE);
    let mut second = HostRegion::new(buddy_region_size(32 * PAGE_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _first_section = init_buddy(&mut buddy, &mut first);
    while buddy.alloc_pages(1, PAGE_SIZE).is_ok() {}
    unsafe { buddy.add_region(second.as_mut_slice()).unwrap() };

    let addr = buddy.alloc_pages(8, PAGE_SIZE).unwrap();
    assert_eq!(
        buddy.allocated_bytes(),
        buddy.managed_bytes() - buddy.free_pages() * PAGE_SIZE
    );
    buddy.dealloc_pages(addr, 8);
    assert_eq!(
        buddy.allocated_bytes(),
        buddy.managed_bytes() - buddy.free_pages() * PAGE_SIZE
    );
}

// ======================================================================
// Slab allocator (standalone) tests
// ======================================================================

#[test]
fn slab_basic() {
    let mut region = HostRegion::new(buddy_region_size(TEST_HEAP_SIZE), PAGE_SIZE);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    let mut slab = SlabAllocator::<PAGE_SIZE>::new();

    let layout = Layout::from_size_align(64, 8).unwrap();
    // First alloc should request pages
    match slab.alloc(layout).unwrap() {
        SlabAllocResult::NeedsSlab { size_class, pages } => {
            let addr = buddy.alloc_pages(pages, PAGE_SIZE).unwrap();
            slab.add_slab(size_class, addr, pages * PAGE_SIZE, 0);
        }
        SlabAllocResult::Allocated(_) => panic!("should need slab first"),
    }

    // Now allocation should succeed
    let ptr = match slab.alloc(layout).unwrap() {
        SlabAllocResult::Allocated(p) => p,
        _ => panic!("expected allocated"),
    };

    // Dealloc
    match slab.dealloc(ptr, layout) {
        SlabDeallocResult::Done => {}
        SlabDeallocResult::FreeSlab { .. } => {} // also valid
    }
}

#[test]
fn slab_many_objects() {
    let mut region = HostRegion::new(buddy_region_size(TEST_HEAP_SIZE) + 0x10000, 0x10000);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy_with_heap_alignment(&mut buddy, &mut region, 0x10000);

    let mut slab = SlabAllocator::<PAGE_SIZE>::new();
    let layout = Layout::from_size_align(32, 8).unwrap();

    let mut ptrs = Vec::new();
    for _ in 0..200 {
        loop {
            match slab.alloc(layout).unwrap() {
                SlabAllocResult::Allocated(p) => {
                    ptrs.push(p);
                    break;
                }
                SlabAllocResult::NeedsSlab { size_class, pages } => {
                    let slab_bytes = pages * PAGE_SIZE;
                    let addr = buddy.alloc_pages(pages, slab_bytes).unwrap();
                    slab.add_slab(size_class, addr, slab_bytes, 0);
                }
            }
        }
    }

    assert_eq!(ptrs.len(), 200);
    for ptr in ptrs {
        let _ = slab.dealloc(ptr, layout);
    }
}

#[test]
fn slab_all_size_classes() {
    let mut region = HostRegion::new(buddy_region_size(TEST_HEAP_SIZE) + 0x10000, 0x10000);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy_with_heap_alignment(&mut buddy, &mut region, 0x10000);

    let mut slab = SlabAllocator::<PAGE_SIZE>::new();
    let mut allocations = Vec::new();

    for sc in SizeClass::ALL {
        let layout = Layout::from_size_align(sc.size(), sc.size()).unwrap();
        loop {
            match slab.alloc(layout).unwrap() {
                SlabAllocResult::Allocated(p) => {
                    allocations.push((p, layout));
                    break;
                }
                SlabAllocResult::NeedsSlab { size_class, pages } => {
                    let slab_bytes = pages * PAGE_SIZE;
                    let addr = buddy.alloc_pages(pages, slab_bytes).unwrap();
                    slab.add_slab(size_class, addr, slab_bytes, 0);
                }
            }
        }
    }

    assert_eq!(allocations.len(), SizeClass::COUNT);
    for (ptr, layout) in allocations {
        let _ = slab.dealloc(ptr, layout);
    }
}

#[test]
fn slab_reuses_freed_objects_same_size_class() {
    let mut region = HostRegion::new(buddy_region_size(TEST_HEAP_SIZE), PAGE_SIZE * 4);
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    let _section = init_buddy(&mut buddy, &mut region);

    let mut slab = SlabAllocator::<PAGE_SIZE>::new();
    let layout = Layout::from_size_align(64, 8).unwrap();
    let (size_class, pages) = match slab.alloc(layout).unwrap() {
        SlabAllocResult::NeedsSlab { size_class, pages } => (size_class, pages),
        SlabAllocResult::Allocated(_) => panic!("should need slab first"),
    };
    let slab_bytes = pages * PAGE_SIZE;
    let addr = buddy.alloc_pages(pages, slab_bytes).unwrap();
    slab.add_slab(size_class, addr, slab_bytes, 0);

    let first = match slab.alloc(layout).unwrap() {
        SlabAllocResult::Allocated(ptr) => ptr,
        SlabAllocResult::NeedsSlab { .. } => panic!("expected allocation from fresh slab"),
    };
    let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(first.as_ptr() as usize, slab_bytes);
    let hdr = unsafe { &*(base as *const SlabPageHeader) };
    let object_count = hdr.object_count as usize;

    let mut ptrs = Vec::with_capacity(object_count);
    ptrs.push(first);
    for _ in 1..object_count {
        let ptr = match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => ptr,
            SlabAllocResult::NeedsSlab { .. } => panic!("expected same slab to satisfy alloc"),
        };
        let ptr_base =
            SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
        assert_eq!(ptr_base, base);
        ptrs.push(ptr);
    }
    assert!(matches!(
        slab.alloc(layout).unwrap(),
        SlabAllocResult::NeedsSlab { .. }
    ));

    let freed_ptrs: Vec<_> = ptrs.iter().copied().step_by(2).collect();
    let freed_addrs: BTreeSet<_> = freed_ptrs.iter().map(|ptr| ptr.as_ptr() as usize).collect();
    for &ptr in &freed_ptrs {
        assert!(matches!(slab.dealloc(ptr, layout), SlabDeallocResult::Done));
    }

    let mut reused_addrs = BTreeSet::new();
    for _ in 0..freed_addrs.len() {
        let ptr = match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => ptr,
            SlabAllocResult::NeedsSlab { .. } => panic!("expected reuse from freed slots"),
        };
        let ptr_base =
            SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
        assert_eq!(ptr_base, base);
        reused_addrs.insert(ptr.as_ptr() as usize);
    }
    assert_eq!(reused_addrs, freed_addrs);

    for ptr in ptrs {
        let addr = ptr.as_ptr() as usize;
        if !freed_addrs.contains(&addr) {
            let _ = slab.dealloc(ptr, layout);
        }
    }
    for addr in reused_addrs {
        let ptr = unsafe { NonNull::new_unchecked(addr as *mut u8) };
        let _ = slab.dealloc(ptr, layout);
    }
}

#[test]
fn global_init_with_unaligned_region_preserves_large_alloc_alignment() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;

    let mut region = irregular_region(12 * ALIGN_2M, 0x1234, PAGE_SIZE / 3, ALIGN_2M);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let region_len = region.len();
    let region_slice = unsafe { region.subslice(0x1234, region_len - 0x1234 - PAGE_SIZE / 3) };

    let _ctx = init_global_slice(&allocator, region_slice, 1);

    let layout = Layout::from_size_align(ALIGN_2M, ALIGN_2M).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    assert_eq!((ptr.as_ptr() as usize) % ALIGN_2M, 0);
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn page_alignment_is_checked_in_the_physical_address_space() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;

    let mut region = HostRegion::new(4 * ALIGN_2M, ALIGN_2M);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global_allocator(&allocator, &mut region, 1);
    set_physical_offset(PAGE_SIZE);

    let addr = allocator.alloc_pages(1, ALIGN_2M).unwrap();
    assert_eq!(virt_to_phys(addr) % ALIGN_2M, 0);
    allocator.dealloc_pages(addr, 1);
}

#[test]
fn global_add_region_with_unaligned_slice_preserves_large_alloc_alignment() {
    const ALIGN_2M: usize = 2 * 1024 * 1024;

    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let mut first = HostRegion::new(12 * ALIGN_2M, ALIGN_2M);
    let _ctx = init_global_allocator(&allocator, &mut first, 1);

    let mut second = irregular_region(12 * ALIGN_2M, 0x1234, PAGE_SIZE / 3, ALIGN_2M);
    let second_len = second.len();
    let second_slice = unsafe { second.subslice(0x1234, second_len - 0x1234 - PAGE_SIZE / 3) };
    unsafe { allocator.add_region(second_slice).unwrap() };

    let layout = Layout::from_size_align(ALIGN_2M, ALIGN_2M).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    assert_eq!((ptr.as_ptr() as usize) % ALIGN_2M, 0);
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_add_region_unaligned_does_not_break_small_alloc() {
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let mut first = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE);
    let _ctx = init_global_allocator(&allocator, &mut first, 1);

    let mut second = irregular_region(
        buddy_region_size(TEST_HEAP_SIZE),
        0x1234,
        PAGE_SIZE / 3,
        PAGE_SIZE,
    );
    let second_len = second.len();
    let second_slice = unsafe { second.subslice(0x1234, second_len - 0x1234 - PAGE_SIZE / 3) };
    unsafe { allocator.add_region(second_slice).unwrap() };

    for layout in [
        Layout::from_size_align(64, 8).unwrap(),
        Layout::from_size_align(128, 16).unwrap(),
        Layout::from_size_align(512, 64).unwrap(),
    ] {
        let ptr = allocator.alloc(layout).unwrap();
        unsafe {
            ptr.as_ptr().write_bytes(0x5a, layout.size());
            allocator.dealloc(ptr, layout);
        }
    }
}

// ======================================================================
// Global allocator tests
// ======================================================================

fn init_global(
    allocator: &GlobalAllocator<PAGE_SIZE>,
    region: &mut HostRegion,
    cpu_count: usize,
) -> GlobalTestContext {
    init_global_allocator(allocator, region, cpu_count)
}

#[test]
fn global_reinit_same_instance_rejected() {
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = global_test_context::<PAGE_SIZE>(1);
    let mut first = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let mut second = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);

    unsafe { allocator.init(first.as_mut_slice()).unwrap() };
    let err = unsafe { allocator.init(second.as_mut_slice()) }.unwrap_err();
    assert_eq!(err, AllocError::AlreadyInitialized);
}

#[test]
fn global_second_live_instance_rejected() {
    let first_allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let second_allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = global_test_context::<PAGE_SIZE>(1);
    let mut first = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let mut second = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);

    unsafe { first_allocator.init(first.as_mut_slice()).unwrap() };
    let err = unsafe { second_allocator.init(second.as_mut_slice()) }.unwrap_err();
    assert_eq!(err, AllocError::AlreadyInitialized);
}

#[test]
fn global_failed_init_rolls_back_singleton() {
    let bad_allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let good_allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = global_test_context::<PAGE_SIZE>(1);
    let mut bad = HostRegion::new(PAGE_SIZE - 1, PAGE_SIZE);
    let mut good = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);

    let err = unsafe { bad_allocator.init(bad.as_mut_slice()) }.unwrap_err();
    assert_eq!(err, AllocError::InvalidParam);
    unsafe { good_allocator.init(good.as_mut_slice()).unwrap() };
}

#[test]
fn global_page_alloc() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let region_addr = region.addr();
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let section = primary_section(&allocator);
    let managed_start = section.start;
    let managed_end = managed_start + section.size;

    let addr = allocator.alloc_pages(4, PAGE_SIZE).unwrap();
    assert!(managed_start > region_addr);
    assert!(addr >= managed_start && addr < managed_end);
    assert_eq!(addr % PAGE_SIZE, 0);
    allocator.dealloc_pages(addr, 4);
}

#[test]
fn global_small_alloc() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let layout = Layout::from_size_align(64, 8).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_large_alloc() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let layout = Layout::from_size_align(8192, PAGE_SIZE).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_mixed_alloc() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let sizes: &[(usize, usize)] = &[
        (8, 8),
        (64, 8),
        (1024, 8),
        (4096, PAGE_SIZE),
        (8192, PAGE_SIZE),
    ];
    let mut allocations = Vec::new();
    for &(size, align) in sizes {
        let layout = Layout::from_size_align(size, align).unwrap();
        let ptr = allocator.alloc(layout).unwrap();
        allocations.push((ptr, layout));
    }
    for (ptr, layout) in allocations {
        unsafe { allocator.dealloc(ptr, layout) };
    }
}

#[test]
fn global_cross_cpu_free() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);

    // Allocate on CPU 0
    set_current_cpu(0);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let mut ptrs = Vec::new();
    for _ in 0..10 {
        ptrs.push(allocator.alloc(layout).unwrap());
    }

    // Free from CPU 1 (triggers remote free path)
    set_current_cpu(1);
    for ptr in ptrs {
        unsafe { allocator.dealloc(ptr, layout) };
    }

    // Allocate on CPU 0 again — should drain remote frees and succeed
    set_current_cpu(0);
    let ptr = allocator.alloc(layout).unwrap();
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_cross_cpu_free_drains_remote_queue() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);

    set_current_cpu(0);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let ptr = allocator.alloc(layout).unwrap();

    let slab_bytes = SizeClass::from_layout(layout)
        .unwrap()
        .slab_pages(PAGE_SIZE)
        * PAGE_SIZE;
    let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
    let hdr = unsafe { &*(base as *const SlabPageHeader) };
    assert_eq!(hdr.owner_cpu, 0);
    assert_eq!(
        hdr.remote_free_count
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );

    set_current_cpu(1);
    unsafe { allocator.dealloc(ptr, layout) };
    assert_eq!(
        hdr.remote_free_count
            .load(core::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_ne!(
        hdr.remote_free_head
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );

    set_current_cpu(0);
    let ptr2 = allocator.alloc(layout).unwrap();
    assert_eq!(
        hdr.remote_free_count
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        hdr.remote_free_head
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    unsafe { allocator.dealloc(ptr2, layout) };
}

#[test]
fn global_cross_cpu_free_multiple_rounds_same_slab() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);

    let layout = Layout::from_size_align(64, 8).unwrap();

    set_current_cpu(0);
    let first = allocator.alloc(layout).unwrap();
    let slab_bytes = SizeClass::from_layout(layout)
        .unwrap()
        .slab_pages(PAGE_SIZE)
        * PAGE_SIZE;
    let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(first.as_ptr() as usize, slab_bytes);
    let hdr = unsafe { &*(base as *const SlabPageHeader) };
    let object_count = hdr.object_count as usize;
    let mut ptrs = Vec::with_capacity(object_count);
    ptrs.push(first);
    for _ in 1..object_count {
        let ptr = allocator.alloc(layout).unwrap();
        let ptr_base =
            SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
        assert_eq!(ptr_base, base);
        ptrs.push(ptr);
    }

    set_current_cpu(1);
    for &ptr in &ptrs {
        unsafe { allocator.dealloc(ptr, layout) };
    }
    assert_eq!(
        hdr.remote_free_count
            .load(core::sync::atomic::Ordering::Relaxed) as usize,
        object_count
    );

    set_current_cpu(0);
    let mut drained = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        drained.push(allocator.alloc(layout).unwrap());
    }
    assert_eq!(
        hdr.remote_free_count
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        hdr.remote_free_head
            .load(core::sync::atomic::Ordering::Relaxed),
        0
    );

    for ptr in drained {
        unsafe { allocator.dealloc(ptr, layout) };
    }
}

#[test]
fn global_full_slab_remote_then_local_free_reuses_without_list_cycle() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);

    let layout = Layout::from_size_align(64, 8).unwrap();

    set_current_cpu(0);
    let first = allocator.alloc(layout).unwrap();
    let slab_bytes = SizeClass::from_layout(layout)
        .unwrap()
        .slab_pages(PAGE_SIZE)
        * PAGE_SIZE;
    let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(first.as_ptr() as usize, slab_bytes);
    let hdr = unsafe { &*(base as *const SlabPageHeader) };
    let object_count = hdr.object_count as usize;

    let mut ptrs = Vec::with_capacity(object_count);
    ptrs.push(first);
    for _ in 1..object_count {
        let ptr = allocator.alloc(layout).unwrap();
        let ptr_base =
            SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
        assert_eq!(ptr_base, base);
        ptrs.push(ptr);
    }
    assert_eq!(hdr.local_free_count, 0);

    let remote_count = 4;
    set_current_cpu(1);
    for &ptr in &ptrs[..remote_count] {
        unsafe { allocator.dealloc(ptr, layout) };
    }
    assert_ne!(
        hdr.remote_free_head
            .load(core::sync::atomic::Ordering::Acquire),
        0
    );

    set_current_cpu(0);
    for &ptr in &ptrs[remote_count..] {
        unsafe { allocator.dealloc(ptr, layout) };
    }
    assert_eq!(
        hdr.remote_free_head
            .load(core::sync::atomic::Ordering::Acquire),
        0
    );

    let mut reused = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let ptr = allocator.alloc(layout).unwrap();
        let ptr_base =
            SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
        assert_eq!(ptr_base, base);
        reused.push(ptr);
    }

    assert_ne!(hdr.list_prev, base, "slab list_prev points to itself");
    assert_ne!(hdr.list_next, base, "slab list_next points to itself");

    for ptr in reused {
        unsafe { allocator.dealloc(ptr, layout) };
    }
}

#[test]
fn global_small_object_churn_then_large_alloc() {
    const REGION_SIZE: usize = 8 * 1024 * 1024;

    let mut region = HostRegion::new(REGION_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let small_layout = Layout::from_size_align(2048, 8).unwrap();
    let warmup = allocator.alloc(small_layout).unwrap();
    unsafe { allocator.dealloc(warmup, small_layout) };
    let baseline = count_free_pages(&allocator);
    let mut ptrs = Vec::new();
    while let Ok(ptr) = allocator.alloc(small_layout) {
        ptrs.push(ptr);
    }
    assert!(!ptrs.is_empty());

    for ptr in ptrs {
        unsafe { allocator.dealloc(ptr, small_layout) };
    }

    let large_layout = Layout::from_size_align(16 * PAGE_SIZE, PAGE_SIZE).unwrap();
    let ptr = allocator.alloc(large_layout).unwrap();
    unsafe { allocator.dealloc(ptr, large_layout) };
    assert_eq!(count_free_pages(&allocator), baseline);
}

#[test]
fn global_cross_cpu_free_all_objects_recovers_backend_pages() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);

    let layout = Layout::from_size_align(64, 8).unwrap();
    set_current_cpu(0);
    let warmup = allocator.alloc(layout).unwrap();
    unsafe { allocator.dealloc(warmup, layout) };
    let baseline = count_free_pages(&allocator);

    let first = allocator.alloc(layout).unwrap();
    let slab_bytes = SizeClass::from_layout(layout)
        .unwrap()
        .slab_pages(PAGE_SIZE)
        * PAGE_SIZE;
    let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(first.as_ptr() as usize, slab_bytes);
    let hdr = unsafe { &*(base as *const SlabPageHeader) };
    let object_count = hdr.object_count as usize;

    let mut ptrs = Vec::with_capacity(object_count);
    ptrs.push(first);
    for _ in 1..object_count {
        ptrs.push(allocator.alloc(layout).unwrap());
    }

    set_current_cpu(1);
    for &ptr in &ptrs {
        unsafe { allocator.dealloc(ptr, layout) };
    }

    set_current_cpu(0);
    let mut drained = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        drained.push(allocator.alloc(layout).unwrap());
    }
    for ptr in drained {
        unsafe { allocator.dealloc(ptr, layout) };
    }

    assert_eq!(count_free_pages(&allocator), baseline);
}

#[test]
fn global_lowmem_fragmentation_recovery() {
    const REGION_SIZE: usize = 8 * 1024 * 1024;

    let mut region = HostRegion::new(REGION_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global_allocator(&allocator, &mut region, 1);

    let mut addrs = Vec::new();
    while let Ok(addr) = allocator.alloc_pages_lowmem(1, PAGE_SIZE) {
        addrs.push(addr);
    }
    assert!(addrs.len() > 8);

    for &addr in addrs.iter().step_by(2) {
        allocator.dealloc_pages(addr, 1);
    }
    assert!(allocator.alloc_pages_lowmem(2, 2 * PAGE_SIZE).is_err());

    for &addr in addrs.iter().skip(1).step_by(2) {
        allocator.dealloc_pages(addr, 1);
    }

    let addr = allocator.alloc_pages_lowmem(2, 2 * PAGE_SIZE).unwrap();
    allocator.dealloc_pages(addr, 2);
}

#[test]
fn global_lowmem_pages() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global_allocator(&allocator, &mut region, 1);

    let addr = allocator.alloc_pages_lowmem(1, PAGE_SIZE).unwrap();
    assert!(addr >= primary_section(&allocator).start);
    allocator.dealloc_pages(addr, 1);
}

#[test]
fn global_unaligned_region_start() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE + PAGE_SIZE, PAGE_SIZE * 4);
    let region_start = region.addr() + 1;
    let region_size = TEST_HEAP_SIZE;
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let unaligned_region = unsafe { region.subslice(1, region_size) };
    let _ctx = init_global_slice(&allocator, unaligned_region, 1);

    let section = primary_section(&allocator);
    let managed_start = section.start;
    let managed_end = managed_start + section.size;

    assert_eq!(managed_start % PAGE_SIZE, 0);
    assert!(managed_start >= region_start);
    assert!(managed_end <= region_start + region_size);

    let addr = allocator.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr >= managed_start && addr < managed_end);
    allocator.dealloc_pages(addr, 1);
}

#[test]
fn global_rejects_region_without_one_managed_page() {
    let region_size = PAGE_SIZE - 1;
    let mut region = HostRegion::new(region_size, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = global_test_context::<PAGE_SIZE>(1);

    let err = unsafe { allocator.init(region.as_mut_slice()) }.unwrap_err();
    assert_eq!(err, AllocError::InvalidParam);
}

#[test]
fn global_add_region_after_init_expands_capacity() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(8 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);

    let before = count_free_pages(&allocator);
    unsafe { allocator.add_region(second.as_mut_slice()).unwrap() };
    let after = count_free_pages(&allocator);

    assert!(after > before);
    assert_eq!(allocator.managed_section_count(), 2);
}

#[test]
fn global_add_region_supports_discontiguous_regions() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(8 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);

    while allocator.alloc_pages(1, PAGE_SIZE).is_ok() {}
    unsafe { allocator.add_region(second.as_mut_slice()).unwrap() };
    let second_section = allocator.managed_section(1).unwrap();

    let addr = allocator.alloc_pages(1, PAGE_SIZE).unwrap();
    assert!(addr >= second_section.start && addr < second_section.start + second_section.size);
}

#[test]
fn global_large_alloc_can_come_from_added_region() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(8 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);

    while allocator.alloc_pages(1, PAGE_SIZE).is_ok() {}
    unsafe { allocator.add_region(second.as_mut_slice()).unwrap() };
    let second_section = allocator.managed_section(1).unwrap();

    let layout = Layout::from_size_align(8 * PAGE_SIZE, PAGE_SIZE).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    let addr = ptr.as_ptr() as usize;
    assert!(addr >= second_section.start && addr < second_section.start + second_section.size);
    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_managed_section_queries_report_all_sections() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(8 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);
    unsafe { allocator.add_region(second.as_mut_slice()).unwrap() };

    assert_eq!(allocator.managed_section_count(), 2);
    let first_section = allocator.managed_section(0).unwrap();
    let second_section = allocator.managed_section(1).unwrap();
    assert!(first_section.size > 0);
    assert!(second_section.size > 0);
}

#[test]
fn global_add_region_overlap_rejected() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);

    let overlap = unsafe { first.subslice(1, first.len() - 1) };
    let err = unsafe { allocator.add_region(overlap) }.unwrap_err();
    assert_eq!(err, AllocError::MemoryOverlap);
}

#[test]
fn global_managed_bytes_matches_all_sections() {
    let mut first = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(8 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, 1);
    unsafe { allocator.add_region(second.as_mut_slice()).unwrap() };

    let expected = (0..allocator.managed_section_count())
        .map(|i| allocator.managed_section(i).unwrap().size)
        .sum::<usize>();
    assert_eq!(allocator.managed_bytes(), expected);
}

#[test]
fn global_allocated_bytes_changes_with_large_alloc() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    assert_eq!(allocator.allocated_bytes(), 0);

    let layout = Layout::from_size_align(3 * PAGE_SIZE, PAGE_SIZE).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    assert_eq!(allocator.allocated_bytes(), 4 * PAGE_SIZE);

    unsafe { allocator.dealloc(ptr, layout) };
    assert_eq!(allocator.allocated_bytes(), 0);
}

#[test]
fn global_allocated_bytes_reflects_slab_pages() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    assert_eq!(allocator.allocated_bytes(), 0);

    let layout = Layout::from_size_align(64, 8).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    assert!(allocator.allocated_bytes() >= PAGE_SIZE);

    unsafe { allocator.dealloc(ptr, layout) };
}

#[test]
fn global_allocated_bytes_not_zero_until_cached_empty_slab_released() {
    let mut region = HostRegion::new(TEST_HEAP_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);

    let layout = Layout::from_size_align(64, 8).unwrap();
    let ptr = allocator.alloc(layout).unwrap();
    let allocated_after_refill = allocator.allocated_bytes();
    assert!(allocated_after_refill >= PAGE_SIZE);

    unsafe { allocator.dealloc(ptr, layout) };

    // One empty slab may remain cached, so backend occupancy need not drop to zero.
    assert!(allocator.allocated_bytes() <= allocated_after_refill);
    assert!(allocator.allocated_bytes() >= PAGE_SIZE);
}
