//! The owner CPU walks its full list only when a cross-CPU free says so.

#![cfg(feature = "host-test")]

extern crate buddy_slab_allocator;
mod common;

use core::{alloc::Layout, ptr::NonNull};

use buddy_slab_allocator::{BuddyAllocator, PerCpuSlab, SizeClass, SlabAllocResult};
use common::HostRegion;

const PAGE_SIZE: usize = 0x1000;
const TEST_HEAP_SIZE: usize = 16 * 1024 * 1024;
const HEAP_ALIGN: usize = 0x10000;

/// Object size chosen so slabs fill quickly and the full list grows fast.
const OBJECT_SIZE: usize = 2048;

fn aligned_buddy(region: &mut HostRegion) -> BuddyAllocator<PAGE_SIZE> {
    let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
    for offset in (0..HEAP_ALIGN).step_by(PAGE_SIZE) {
        if region.len() <= offset {
            break;
        }
        let slice = unsafe { region.subslice(offset, region.len() - offset) };
        if unsafe { buddy.init(slice) }.is_ok()
            && let Some(section) = buddy.section(0)
            && section.start.is_multiple_of(HEAP_ALIGN)
        {
            return buddy;
        }
        buddy = BuddyAllocator::<PAGE_SIZE>::new();
    }
    panic!("no heap alignment produced an aligned section");
}

fn test_region() -> HostRegion {
    let meta = BuddyAllocator::<PAGE_SIZE>::required_meta_size(TEST_HEAP_SIZE);
    HostRegion::new(
        TEST_HEAP_SIZE + meta + PAGE_SIZE * 4 + HEAP_ALIGN,
        HEAP_ALIGN,
    )
}

/// Allocate one object, feeding the slab pages until it can serve the request.
fn alloc_one(
    slab: &PerCpuSlab<PAGE_SIZE>,
    buddy: &mut BuddyAllocator<PAGE_SIZE>,
    layout: Layout,
) -> NonNull<u8> {
    loop {
        match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => return ptr,
            SlabAllocResult::NeedsSlab { size_class, pages } => {
                let bytes = pages * PAGE_SIZE;
                let base = buddy.alloc_pages(pages, bytes).unwrap();
                slab.add_slab(size_class, base, bytes);
            }
        }
    }
}

/// Allocate until the current slab is full and return its objects.
fn fill_slab(
    slab: &PerCpuSlab<PAGE_SIZE>,
    buddy: &mut BuddyAllocator<PAGE_SIZE>,
    layout: Layout,
) -> Vec<NonNull<u8>> {
    let mut objects = vec![alloc_one(slab, buddy, layout)];
    while let SlabAllocResult::Allocated(ptr) = slab.alloc(layout).unwrap() {
        objects.push(ptr);
    }
    objects
}

#[test]
fn allocation_without_remote_frees_leaves_the_full_list_alone() {
    let mut region = test_region();
    let mut buddy = aligned_buddy(&mut region);
    let slab = PerCpuSlab::<PAGE_SIZE>::new(0);
    let layout = Layout::from_size_align(OBJECT_SIZE, 8).unwrap();
    let class = SizeClass::from_layout(layout).unwrap();

    // Every slab handed out here fills up and moves to the full list, so an
    // unconditional reclaim walk would grow with each allocation.
    let mut live = Vec::new();
    for _ in 0..512 {
        live.push(alloc_one(&slab, &mut buddy, layout));
    }

    assert_eq!(
        slab.full_walk_steps(class),
        0,
        "no cross-CPU free happened, so the full list holds nothing to reclaim",
    );
}

#[test]
fn an_announced_remote_free_comes_back_to_the_owner() {
    let mut region = test_region();
    let mut buddy = aligned_buddy(&mut region);
    let slab = PerCpuSlab::<PAGE_SIZE>::new(0);
    let layout = Layout::from_size_align(OBJECT_SIZE, 8).unwrap();
    let class = SizeClass::from_layout(layout).unwrap();

    let mut live = Vec::new();
    for _ in 0..64 {
        live.push(alloc_one(&slab, &mut buddy, layout));
    }
    // Drain to the point where nothing is partial, so the reclaim walk is the
    // only way back to a free object.
    while let SlabAllocResult::Allocated(ptr) = slab.alloc(layout).unwrap() {
        live.push(ptr);
    }

    let victim = live[0];
    slab.dealloc_remote(victim);

    match slab.alloc(layout).unwrap() {
        SlabAllocResult::Allocated(ptr) => assert_eq!(
            ptr.as_ptr(),
            victim.as_ptr(),
            "the reclaim walk must hand back the remotely freed object",
        ),
        SlabAllocResult::NeedsSlab { .. } => {
            panic!("the announced remote free was never found")
        }
    }
    assert!(
        slab.full_walk_steps(class) > 0,
        "finding the freed object requires walking the full list once",
    );
}

#[test]
fn a_partial_slab_hit_does_not_swallow_a_pending_remote_free() {
    let mut region = test_region();
    let mut buddy = aligned_buddy(&mut region);
    let slab = PerCpuSlab::<PAGE_SIZE>::new(0);
    // Small objects so one slab holds several: the point of this test is a
    // cache that has a full slab and a partial slab with room at the same time.
    let layout = Layout::from_size_align(64, 8).unwrap();

    // Take one object so a first slab exists at all - a fresh cache holds none
    // and would otherwise ask for a slab before allocating anything - then fill
    // that slab exactly and queue a fresh one behind it.
    let mut first = Vec::new();
    first.push(alloc_one(&slab, &mut buddy, layout));
    loop {
        match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => first.push(ptr),
            SlabAllocResult::NeedsSlab { size_class, pages } => {
                let bytes = pages * PAGE_SIZE;
                let base = buddy.alloc_pages(pages, bytes).unwrap();
                slab.add_slab(size_class, base, bytes);
                break;
            }
        }
    }
    // One object out of the fresh slab leaves it partial with room left.
    let _held = alloc_one(&slab, &mut buddy, layout);

    // A cross-CPU free lands on the slab that is already full.
    let victim = first[0];
    slab.dealloc_remote(victim);

    // This allocation is satisfied by the partial slab, so it never reaches the
    // reclaim walk. Consuming the announcement here would strand the freed
    // object in the full list.
    let from_partial = alloc_one(&slab, &mut buddy, layout);
    assert_ne!(
        from_partial.as_ptr(),
        victim.as_ptr(),
        "this allocation should have come from the partial slab",
    );

    // Drain what is left without handing over more slabs: the freed object has
    // to come back out of the full list once the partial slab is spent.
    let mut recovered = false;
    for _ in 0..4096 {
        match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) => {
                if ptr.as_ptr() == victim.as_ptr() {
                    recovered = true;
                    break;
                }
            }
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }
    assert!(
        recovered,
        "the remote free was announced before a partial-slab hit and then lost",
    );
}

#[test]
fn one_walk_reclaims_every_full_slab_with_remote_frees() {
    let mut region = test_region();
    let mut buddy = aligned_buddy(&mut region);
    let slab = PerCpuSlab::<PAGE_SIZE>::new(0);
    let layout = Layout::from_size_align(64, 8).unwrap();

    // Two full slabs of one class, then a third left partial with room.
    let first = fill_slab(&slab, &mut buddy, layout);
    let second = fill_slab(&slab, &mut buddy, layout);
    let _held = alloc_one(&slab, &mut buddy, layout);

    // Both full slabs take a cross-CPU free before the owner allocates again.
    let victims = [first[0], second[0]];
    for victim in victims {
        slab.dealloc_remote(victim);
    }

    // Spend the partial slab, then keep going without new slabs: both freed
    // objects must come back although a single walk consumed the announcement.
    let mut recovered = Vec::new();
    for _ in 0..4096 {
        match slab.alloc(layout).unwrap() {
            SlabAllocResult::Allocated(ptr) if victims.contains(&ptr) => recovered.push(ptr),
            SlabAllocResult::Allocated(_) => {}
            SlabAllocResult::NeedsSlab { .. } => break,
        }
    }
    assert_eq!(
        recovered.len(),
        victims.len(),
        "a remote free on another full slab was stranded after the walk",
    );
}
