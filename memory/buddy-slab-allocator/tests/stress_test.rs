//! Stress tests for allocator stability.

mod common;

use std::{
    alloc::Layout,
    sync::{
        Barrier, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use buddy_slab_allocator::{GlobalAllocator, SizeClass};
use common::{
    HostRegion, count_free_pages, init_global, nonnull_from_addr, seeded_rng, set_current_cpu,
};
use rand::RngExt;

const PAGE_SIZE: usize = 0x1000;
const HEAP_SIZE: usize = 64 * 1024 * 1024;
const WORKERS: usize = 4;

fn assert_recovered_with_cached_slabs(
    allocator: &GlobalAllocator<PAGE_SIZE>,
    baseline: usize,
    cpu_count: usize,
    cached_classes: &[SizeClass],
) {
    let recovered = count_free_pages(allocator);
    let retained_pages = cached_classes
        .iter()
        .map(|sc| sc.slab_pages(PAGE_SIZE))
        .sum::<usize>()
        * cpu_count;
    assert!(
        recovered + retained_pages >= baseline,
        "recovered {recovered} pages, baseline {baseline}, retained allowance {retained_pages}",
    );
}

#[test]
#[ignore = "stress test"]
fn stress_random_mixed_alloc_free() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);
    let mut rng = seeded_rng(0);
    let mut allocated: Vec<(usize, Layout)> = Vec::new();

    for i in 0..10_000 {
        set_current_cpu(i % 2);
        if allocated.is_empty() || rng.random_bool(0.65) {
            let size: usize = rng.random_range(8..8193);
            let layout = if size <= 2048 {
                Layout::from_size_align(size.next_power_of_two().min(2048), 8).unwrap()
            } else {
                let aligned = size.div_ceil(PAGE_SIZE) * PAGE_SIZE;
                Layout::from_size_align(aligned, PAGE_SIZE).unwrap()
            };

            if let Ok(ptr) = allocator.alloc(layout) {
                allocated.push((ptr.as_ptr() as usize, layout));
            }
        } else {
            let idx = rng.random_range(0..allocated.len());
            let (addr, layout) = allocated.swap_remove(idx);
            unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
        }
    }

    for (addr, layout) in allocated {
        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
    }
}

#[test]
#[ignore = "stress test"]
fn stress_exhaustion_recovery() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 1);
    let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
    let mut allocated = Vec::new();

    while let Ok(ptr) = allocator.alloc(layout) {
        allocated.push(ptr.as_ptr() as usize);
    }

    for addr in allocated.drain(..allocated.len() / 4) {
        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
    }

    let recovered = allocator.alloc(layout);
    assert!(recovered.is_ok());

    if let Ok(ptr) = recovered {
        unsafe { allocator.dealloc(ptr, layout) };
    }

    for addr in allocated {
        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
    }
}

#[test]
#[ignore = "stress test"]
fn stress_fragmentation_recovery() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, 2);
    let small_layout = Layout::from_size_align(64, 8).unwrap();
    let mut small_ptrs = Vec::new();

    for i in 0..4000 {
        set_current_cpu(i % 2);
        if let Ok(ptr) = allocator.alloc(small_layout) {
            small_ptrs.push(ptr.as_ptr() as usize);
        }
    }

    for i in (0..small_ptrs.len()).step_by(2) {
        unsafe { allocator.dealloc(nonnull_from_addr(small_ptrs[i]), small_layout) };
    }

    let large_layout = Layout::from_size_align(PAGE_SIZE * 16, PAGE_SIZE).unwrap();
    let large = allocator.alloc(large_layout);

    for addr in small_ptrs.into_iter().skip(1).step_by(2) {
        unsafe { allocator.dealloc(nonnull_from_addr(addr), small_layout) };
    }

    if let Ok(ptr) = large {
        unsafe { allocator.dealloc(ptr, large_layout) };
    }
}

#[test]
#[ignore = "stress test"]
fn stress_multithread_mixed_alloc_free() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, WORKERS);
    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            scope.spawn(move || {
                set_current_cpu(cpu);
                barrier.wait();

                let mut rng = seeded_rng(0x1000 + cpu as u64);
                let mut live: Vec<(usize, Layout)> = Vec::new();
                for _ in 0..4_000 {
                    if live.is_empty() || rng.random_bool(0.65) {
                        let layout = if rng.random_bool(0.7) {
                            let size: usize = rng.random_range(8..=2048);
                            Layout::from_size_align(size.next_power_of_two().min(2048), 8).unwrap()
                        } else {
                            let page_counts = [1usize, 2, 4, 8];
                            let pages = page_counts[rng.random_range(0..page_counts.len())];
                            Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap()
                        };

                        if let Ok(ptr) = allocator.alloc(layout) {
                            live.push((ptr.as_ptr() as usize, layout));
                        }
                    } else {
                        let idx = rng.random_range(0..live.len());
                        let (addr, layout) = live.swap_remove(idx);
                        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                    }
                }

                for (addr, layout) in live {
                    unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                }
            });
        }
    });

    assert_recovered_with_cached_slabs(allocator, baseline, WORKERS, &SizeClass::ALL);
}

#[test]
#[ignore = "stress test"]
fn stress_multithread_remote_free() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, WORKERS);
    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let queues: Vec<_> = (0..WORKERS)
        .map(|_| Mutex::new(Vec::<usize>::new()))
        .collect();

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            let queues = &queues;
            scope.spawn(move || {
                set_current_cpu(cpu);
                let mut local = Vec::new();
                for _ in 0..256 {
                    local.push(allocator.alloc(layout).unwrap().as_ptr() as usize);
                }

                let target = (cpu + 1) % WORKERS;
                queues[target].lock().unwrap().extend(local);
                barrier.wait();

                let remote = {
                    let mut queue = queues[cpu].lock().unwrap();
                    queue.drain(..).collect::<Vec<_>>()
                };
                for addr in remote {
                    unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                }
                barrier.wait();

                let mut drained = Vec::new();
                for _ in 0..256 {
                    drained.push(allocator.alloc(layout).unwrap());
                }
                for ptr in drained {
                    unsafe { allocator.dealloc(ptr, layout) };
                }
                barrier.wait();
            });
        }
    });

    assert_recovered_with_cached_slabs(allocator, baseline, WORKERS, &[SizeClass::Bytes64]);
}

#[test]
#[ignore = "stress test"]
fn stress_multithread_page_alloc_free() {
    let mut region = HostRegion::new(HEAP_SIZE, PAGE_SIZE);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, WORKERS);
    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            scope.spawn(move || {
                set_current_cpu(cpu);
                barrier.wait();

                let mut rng = seeded_rng(0x2000 + cpu as u64);
                let page_counts = [1usize, 2, 4, 8];
                let alignments = [PAGE_SIZE, 2 * PAGE_SIZE, 4 * PAGE_SIZE, 8 * PAGE_SIZE];
                let mut live = Vec::new();

                for _ in 0..2_000 {
                    if live.is_empty() || rng.random_bool(0.6) {
                        let count = page_counts[rng.random_range(0..page_counts.len())];
                        let align = alignments[rng.random_range(0..alignments.len())];
                        if let Ok(addr) = allocator.alloc_pages(count, align.max(PAGE_SIZE)) {
                            live.push((addr, count));
                        }
                    } else {
                        let idx = rng.random_range(0..live.len());
                        let (addr, count) = live.swap_remove(idx);
                        allocator.dealloc_pages(addr, count);
                    }
                }

                for (addr, count) in live {
                    allocator.dealloc_pages(addr, count);
                }
            });
        }
    });

    assert_eq!(count_free_pages(allocator), baseline);
}

#[test]
#[ignore = "stress test"]
fn stress_multithread_fragmentation_recovery() {
    const REGION_SIZE: usize = 4 * 1024 * 1024;

    let mut region = HostRegion::new(REGION_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, WORKERS);
    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);
    let before_cleanup_failed = AtomicBool::new(false);
    let partial_cleanup_failed = AtomicBool::new(false);
    let large_layout = Layout::from_size_align(32 * PAGE_SIZE, PAGE_SIZE).unwrap();
    let small_layout = Layout::from_size_align(64, 8).unwrap();

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            let before_cleanup_failed = &before_cleanup_failed;
            let partial_cleanup_failed = &partial_cleanup_failed;
            scope.spawn(move || {
                set_current_cpu(cpu);
                let mut live = Vec::new();
                while let Ok(ptr) = allocator.alloc(small_layout) {
                    live.push(ptr.as_ptr() as usize);
                }

                barrier.wait();
                if cpu == 0 {
                    match allocator.alloc(large_layout) {
                        Ok(ptr) => unsafe {
                            allocator.dealloc(ptr, large_layout);
                        },
                        Err(_) => before_cleanup_failed.store(true, Ordering::Relaxed),
                    }
                }

                barrier.wait();
                let mut retained = Vec::new();
                for (idx, addr) in live.into_iter().enumerate() {
                    if idx % 2 == 0 {
                        unsafe { allocator.dealloc(nonnull_from_addr(addr), small_layout) };
                    } else {
                        retained.push(addr);
                    }
                }

                barrier.wait();
                if cpu == 0 {
                    match allocator.alloc(large_layout) {
                        Ok(ptr) => unsafe {
                            allocator.dealloc(ptr, large_layout);
                        },
                        Err(_) => partial_cleanup_failed.store(true, Ordering::Relaxed),
                    }
                }

                barrier.wait();
                for addr in retained {
                    unsafe { allocator.dealloc(nonnull_from_addr(addr), small_layout) };
                }

                barrier.wait();
                if cpu == 0 {
                    let ptr = allocator.alloc(large_layout).unwrap();
                    unsafe { allocator.dealloc(ptr, large_layout) };
                }
                barrier.wait();
            });
        }
    });

    assert!(before_cleanup_failed.load(Ordering::Relaxed));
    assert!(partial_cleanup_failed.load(Ordering::Relaxed));
    assert_recovered_with_cached_slabs(allocator, baseline, WORKERS, &[SizeClass::Bytes64]);
}

#[test]
#[ignore = "stress test"]
fn stress_multithread_exhaustion_recovery() {
    const REGION_SIZE: usize = 8 * 1024 * 1024;

    let mut region = HostRegion::new(REGION_SIZE, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut region, WORKERS);
    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);
    let exhausted = AtomicBool::new(false);
    let recovered = AtomicBool::new(false);
    let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            let exhausted = &exhausted;
            let recovered = &recovered;
            scope.spawn(move || {
                set_current_cpu(cpu);
                let mut live = Vec::new();
                while let Ok(ptr) = allocator.alloc(layout) {
                    live.push(ptr.as_ptr() as usize);
                }

                barrier.wait();
                if cpu == 0 {
                    exhausted.store(allocator.alloc(layout).is_err(), Ordering::Relaxed);
                }

                barrier.wait();
                let mut retained = Vec::new();
                for (idx, addr) in live.into_iter().enumerate() {
                    if idx % 4 == 0 {
                        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                    } else {
                        retained.push(addr);
                    }
                }

                barrier.wait();
                if cpu == 0
                    && let Ok(ptr) = allocator.alloc(layout)
                {
                    recovered.store(true, Ordering::Relaxed);
                    unsafe { allocator.dealloc(ptr, layout) };
                }

                barrier.wait();
                for addr in retained {
                    unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                }
                barrier.wait();
            });
        }
    });

    assert!(exhausted.load(Ordering::Relaxed));
    assert!(recovered.load(Ordering::Relaxed));
    assert_eq!(count_free_pages(allocator), baseline);
}

#[test]
#[ignore = "stress test"]
fn stress_add_region_then_multithread_alloc_free() {
    let mut first = HostRegion::new(2 * 1024 * 1024, PAGE_SIZE * 4);
    let mut second = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let mut third = HostRegion::new(4 * 1024 * 1024, PAGE_SIZE * 4);
    let allocator = GlobalAllocator::<PAGE_SIZE>::new();
    let _ctx = init_global(&allocator, &mut first, WORKERS);
    unsafe {
        allocator.add_region(second.as_mut_slice()).unwrap();
        allocator.add_region(third.as_mut_slice()).unwrap();
    }
    assert_eq!(allocator.managed_section_count(), 3);

    let baseline = count_free_pages(&allocator);
    let allocator = &allocator;
    let barrier = Barrier::new(WORKERS);

    thread::scope(|scope| {
        for cpu in 0..WORKERS {
            let barrier = &barrier;
            scope.spawn(move || {
                set_current_cpu(cpu);
                barrier.wait();

                let mut rng = seeded_rng(0x3000 + cpu as u64);
                let mut live: Vec<(usize, Layout)> = Vec::new();
                for _ in 0..5_000 {
                    if live.is_empty() || rng.random_bool(0.65) {
                        let layout = if rng.random_bool(0.75) {
                            let size: usize = rng.random_range(8..=2048);
                            Layout::from_size_align(size.next_power_of_two().min(2048), 8).unwrap()
                        } else {
                            let pages = [1usize, 2, 4, 8][rng.random_range(0..4)];
                            Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap()
                        };

                        if let Ok(ptr) = allocator.alloc(layout) {
                            live.push((ptr.as_ptr() as usize, layout));
                        }
                    } else {
                        let idx = rng.random_range(0..live.len());
                        let (addr, layout) = live.swap_remove(idx);
                        unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                    }
                }

                for (addr, layout) in live {
                    unsafe { allocator.dealloc(nonnull_from_addr(addr), layout) };
                }
            });
        }
    });

    assert_eq!(allocator.managed_section_count(), 3);
    assert_recovered_with_cached_slabs(allocator, baseline, WORKERS, &SizeClass::ALL);
}
