//! Benchmarks for the buddy page allocator.

mod common;

use common::{
    BuddyHarness, FRAGMENTATION_PAGES, HEAP_SIZE, OPERATIONS_PER_BATCH, PAGE_SIZE, seeded_rng,
};
use divan::{Bencher, black_box};
use rand::RngExt;

fn main() {
    divan::main();
}

#[divan::bench_group]
mod buddy {
    use super::*;

    #[divan::bench(args = [1usize, 2, 4, 16, 64])]
    fn page_alloc_free(bencher: Bencher, pages: usize) {
        let mut harness = BuddyHarness::new(HEAP_SIZE);

        bencher.bench_local(|| {
            let addr = harness
                .allocator
                .alloc_pages(black_box(pages), PAGE_SIZE)
                .unwrap();
            harness.allocator.dealloc_pages(addr, pages);
            black_box(addr)
        });
    }

    #[divan::bench(args = [PAGE_SIZE, PAGE_SIZE * 2, PAGE_SIZE * 4, PAGE_SIZE * 16])]
    fn aligned_page_alloc_free(bencher: Bencher, align: usize) {
        let mut harness = BuddyHarness::new(HEAP_SIZE);

        bencher.bench_local(|| {
            let addr = harness
                .allocator
                .alloc_pages(black_box(4), black_box(align))
                .unwrap();
            harness.allocator.dealloc_pages(addr, 4);
            black_box(addr)
        });
    }

    #[divan::bench]
    fn fragmentation_recovery_cycle(bencher: Bencher) {
        let mut harness = BuddyHarness::new(HEAP_SIZE);

        bencher.bench_local(|| {
            let mut addrs = Vec::with_capacity(FRAGMENTATION_PAGES);
            for _ in 0..FRAGMENTATION_PAGES {
                addrs.push(harness.allocator.alloc_pages(1, PAGE_SIZE).unwrap());
            }

            for idx in (0..addrs.len()).step_by(2) {
                harness.allocator.dealloc_pages(addrs[idx], 1);
            }

            let large = harness.allocator.alloc_pages(64, PAGE_SIZE).unwrap();
            harness.allocator.dealloc_pages(large, 64);

            for idx in (1..addrs.len()).step_by(2) {
                harness.allocator.dealloc_pages(addrs[idx], 1);
            }

            black_box(large)
        });
    }

    #[divan::bench]
    fn random_page_workload(bencher: Bencher) {
        let mut harness = BuddyHarness::new(HEAP_SIZE);
        let mut rng = seeded_rng();
        let plan: Vec<(bool, usize, usize)> = (0..OPERATIONS_PER_BATCH)
            .map(|_| {
                let allocate = rng.random_bool(0.65);
                let pages = 1usize << rng.random_range(0..=4);
                let free_hint = rng.random_range(0..OPERATIONS_PER_BATCH.max(1));
                (allocate, pages, free_hint)
            })
            .collect();

        bencher.bench_local(|| {
            let mut active = Vec::new();

            for &(allocate, pages, free_hint) in &plan {
                if allocate || active.is_empty() {
                    if let Ok(addr) = harness.allocator.alloc_pages(pages, PAGE_SIZE) {
                        active.push((addr, pages));
                    }
                } else {
                    let idx = free_hint % active.len();
                    let (addr, count) = active.swap_remove(idx);
                    harness.allocator.dealloc_pages(addr, count);
                }
            }

            for (addr, count) in active {
                harness.allocator.dealloc_pages(addr, count);
            }
        });
    }
}
