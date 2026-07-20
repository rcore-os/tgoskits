use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_runtime::maintenance::{
    MAINTENANCE_BATCH_LIMIT, MaintenanceCauses, MaintenanceMailbox, MaintenancePublishResult,
};

struct TrackingAllocator;

std::thread_local! {
    static TRACK_ALLOCATION: Cell<bool> = const { Cell::new(false) };
}

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

// SAFETY: every operation forwards the allocator contract unchanged; the
// thread-local switch only observes operations on this test thread.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(&ALLOCATIONS);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record(&DEALLOCATIONS);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(&ALLOCATIONS);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(&ALLOCATIONS);
        record(&DEALLOCATIONS);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[test]
fn irq_mailbox_publication_and_owner_drain_allocate_and_free_nothing() {
    let mailbox = MaintenanceMailbox::<u64>::new();
    assert_no_alloc_or_free(|| {
        assert_eq!(
            mailbox.publish_irq_event_serialized(MaintenanceCauses::IRQ, 7),
            MaintenancePublishResult::Published
        );
    });

    let observed = AtomicUsize::new(0);
    assert_no_alloc_or_free(|| {
        let drain = mailbox
            .drain_owner(MAINTENANCE_BATCH_LIMIT, |event| {
                observed.store(event as usize, Ordering::Relaxed);
            })
            .unwrap();
        assert_eq!(drain.drained(), 1);
    });
    assert_eq!(observed.load(Ordering::Relaxed), 7);
}

fn record(counter: &AtomicUsize) {
    let _result = TRACK_ALLOCATION.try_with(|tracking| {
        if tracking.get() {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    });
}

fn assert_no_alloc_or_free<T>(operation: impl FnOnce() -> T) -> T {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    TRACK_ALLOCATION.with(|tracking| tracking.set(true));
    let output = operation();
    TRACK_ALLOCATION.with(|tracking| tracking.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(DEALLOCATIONS.load(Ordering::Relaxed), 0);
    output
}
