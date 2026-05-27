//! Benchmarks for the slab allocator with buddy-backed slab refill.

mod common;

use core::alloc::Layout;

use common::{HEAP_SIZE, OPERATIONS_PER_BATCH, SlabHarness};
use divan::{Bencher, black_box};

fn main() {
    divan::main();
}

#[divan::bench_group]
mod slab {
    use super::*;

    #[divan::bench(args = [8usize, 64, 256, 512, 1024, 2048])]
    fn size_class_alloc_free(bencher: Bencher, size: usize) {
        let mut harness = SlabHarness::new(HEAP_SIZE);
        let layout = Layout::from_size_align(size, size).unwrap();

        bencher.bench_local(|| {
            let ptr = harness.alloc(black_box(layout));
            harness.dealloc(ptr, layout);
            black_box(ptr)
        });
    }

    #[divan::bench]
    fn hot_reuse(bencher: Bencher) {
        let mut harness = SlabHarness::new(HEAP_SIZE);
        let layout = Layout::from_size_align(128, 128).unwrap();
        let ptr = harness.alloc(layout);
        harness.dealloc(ptr, layout);

        bencher.bench_local(|| {
            let ptr = harness.alloc(layout);
            harness.dealloc(ptr, layout);
            black_box(ptr)
        });
    }

    #[divan::bench]
    fn mixed_size_batch(bencher: Bencher) {
        let mut harness = SlabHarness::new(HEAP_SIZE);
        let layouts = [
            Layout::from_size_align(8, 8).unwrap(),
            Layout::from_size_align(64, 64).unwrap(),
            Layout::from_size_align(256, 256).unwrap(),
            Layout::from_size_align(512, 512).unwrap(),
            Layout::from_size_align(1024, 1024).unwrap(),
            Layout::from_size_align(2048, 2048).unwrap(),
        ];

        bencher.bench_local(|| {
            for layout in layouts {
                let ptr = harness.alloc(layout);
                harness.dealloc(ptr, layout);
                black_box(ptr);
            }
        });
    }

    #[divan::bench]
    fn steady_state_recycle(bencher: Bencher) {
        let mut harness = SlabHarness::new(HEAP_SIZE);
        let layout = Layout::from_size_align(64, 64).unwrap();
        let mut active = Vec::with_capacity(256);
        for _ in 0..256 {
            active.push(harness.alloc(layout));
        }

        bencher.bench_local(|| {
            let ptr = active.pop().unwrap();
            harness.dealloc(ptr, layout);
            let new_ptr = harness.alloc(layout);
            active.push(new_ptr);
            black_box(new_ptr)
        });
    }

    #[divan::bench]
    fn mixed_size_workload(bencher: Bencher) {
        let mut harness = SlabHarness::new(HEAP_SIZE);
        let layouts = [
            Layout::from_size_align(8, 8).unwrap(),
            Layout::from_size_align(64, 64).unwrap(),
            Layout::from_size_align(256, 256).unwrap(),
            Layout::from_size_align(1024, 1024).unwrap(),
        ];

        bencher.bench_local(|| {
            for idx in 0..OPERATIONS_PER_BATCH {
                let layout = layouts[idx % layouts.len()];
                let ptr = harness.alloc(layout);
                harness.dealloc(ptr, layout);
                black_box(ptr);
            }
        });
    }
}
