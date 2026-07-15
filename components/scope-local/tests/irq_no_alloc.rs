use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

use scope_local::scope_local;

mod support;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: every operation delegates to System with the original pointer and
// layout. The counter is diagnostic state and does not affect allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded GlobalAlloc contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded GlobalAlloc contract.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded GlobalAlloc contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded GlobalAlloc contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

scope_local! {
    static IRQ_VALUE: usize = 9;
}

#[test]
fn first_pinned_irq_lookup_does_not_initialize_or_allocate() {
    ax_percpu::init();
    let cpu = ax_percpu::CpuIndex::try_from(0).unwrap();
    let area = ax_percpu::area(cpu).unwrap();
    support::bind_test_area(area);
    // SAFETY: this single-threaded test binds one CPU area and cannot migrate
    // during the immediately following lookup.
    let pin = unsafe { ax_percpu::CpuPin::new_unchecked() };
    let before = ALLOCATIONS.load(Ordering::Relaxed);

    let value = IRQ_VALUE.try_with_pinned(&pin, |value| *value);

    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(value, None);
    assert_eq!(after, before, "hard-IRQ lookup must not allocate");
}
