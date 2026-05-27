//! Benchmarks for the unified global allocator.

mod common;

use core::alloc::Layout;

use common::{GlobalHarness, HEAP_SIZE, MOCK_OS, OPERATIONS_PER_BATCH, PAGE_SIZE};
use divan::{Bencher, black_box};

fn main() {
    divan::main();
}

#[divan::bench_group]
mod global {
    use super::*;

    #[divan::bench(args = [8usize, 64, 512, 2048])]
    fn small_alloc_free(bencher: Bencher, size: usize) {
        let harness = GlobalHarness::new(HEAP_SIZE, 2);
        let layout = Layout::from_size_align(size, 8).unwrap();

        bencher.bench_local(|| {
            MOCK_OS.set_cpu(0);
            let ptr = harness.allocator.alloc(black_box(layout)).unwrap();
            unsafe { harness.allocator.dealloc(ptr, layout) };
            black_box(ptr)
        });
    }

    #[divan::bench(args = [PAGE_SIZE, PAGE_SIZE * 4, PAGE_SIZE * 16])]
    fn large_alloc_free(bencher: Bencher, size: usize) {
        let harness = GlobalHarness::new(HEAP_SIZE, 2);
        let layout = Layout::from_size_align(size, PAGE_SIZE).unwrap();

        bencher.bench_local(|| {
            MOCK_OS.set_cpu(0);
            let ptr = harness.allocator.alloc(black_box(layout)).unwrap();
            unsafe { harness.allocator.dealloc(ptr, layout) };
            black_box(ptr)
        });
    }

    #[divan::bench(args = [1usize, 4, 16, 64])]
    fn page_interface(bencher: Bencher, pages: usize) {
        let harness = GlobalHarness::new(HEAP_SIZE, 2);

        bencher.bench_local(|| {
            MOCK_OS.set_cpu(0);
            let addr = harness
                .allocator
                .alloc_pages(black_box(pages), PAGE_SIZE)
                .unwrap();
            harness.allocator.dealloc_pages(addr, pages);
            black_box(addr)
        });
    }

    #[divan::bench]
    fn mixed_object_page_workload(bencher: Bencher) {
        let harness = GlobalHarness::new(HEAP_SIZE, 2);
        let object_layouts = [
            Layout::from_size_align(64, 8).unwrap(),
            Layout::from_size_align(256, 8).unwrap(),
            Layout::from_size_align(1024, 8).unwrap(),
            Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap(),
        ];
        let page_counts = [1usize, 2, 4, 8];

        bencher.bench_local(|| {
            for idx in 0..OPERATIONS_PER_BATCH {
                MOCK_OS.set_cpu(idx % 2);
                let layout = object_layouts[idx % object_layouts.len()];
                let ptr = harness.allocator.alloc(layout).unwrap();
                unsafe { harness.allocator.dealloc(ptr, layout) };

                let pages = page_counts[idx % page_counts.len()];
                let addr = harness.allocator.alloc_pages(pages, PAGE_SIZE).unwrap();
                harness.allocator.dealloc_pages(addr, pages);

                black_box((ptr, addr));
            }
        });
    }

    #[divan::bench]
    fn remote_free_cycle(bencher: Bencher) {
        let harness = GlobalHarness::new(HEAP_SIZE, 2);
        let layout = Layout::from_size_align(64, 8).unwrap();

        bencher.bench_local(|| {
            let mut ptrs = Vec::with_capacity(64);

            MOCK_OS.set_cpu(0);
            for _ in 0..64 {
                ptrs.push(harness.allocator.alloc(layout).unwrap());
            }

            MOCK_OS.set_cpu(1);
            for &ptr in &ptrs {
                unsafe { harness.allocator.dealloc(ptr, layout) };
            }

            MOCK_OS.set_cpu(0);
            for _ in 0..64 {
                let ptr = harness.allocator.alloc(layout).unwrap();
                unsafe { harness.allocator.dealloc(ptr, layout) };
                black_box(ptr);
            }
        });
    }
}
