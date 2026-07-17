use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_runtime::workqueue::{
    QueueWorkResult, WorkItem, WorkOutcome, WorkPriority, WorkQueueSystem,
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
// thread-local flag only observes the current test thread.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe {
            // SAFETY: this wrapper forwards the allocator contract unchanged.
            System.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_deallocation();
        unsafe {
            // SAFETY: `pointer` and `layout` are forwarded to their allocator.
            System.dealloc(pointer, layout);
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe {
            // SAFETY: this wrapper forwards the allocator contract unchanged.
            System.alloc_zeroed(layout)
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        record_deallocation();
        unsafe {
            // SAFETY: all arguments are forwarded to their allocator.
            System.realloc(pointer, layout, new_size)
        }
    }
}

fn record_allocation() {
    let _ = TRACK_ALLOCATION.try_with(|tracking| {
        if tracking.get() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
    });
}

fn record_deallocation() {
    let _ = TRACK_ALLOCATION.try_with(|tracking| {
        if tracking.get() {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
    });
}

#[test]
fn queue_service_and_cancel_paths_allocate_and_free_nothing() {
    let queue = Box::leak(Box::new(WorkQueueSystem::<1>::new()));
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        count_callback,
        ptr::from_ref(calls).expose_provenance(),
    ))));

    assert_no_alloc_or_free(|| {
        assert_eq!(
            queue.queue_work_on(0, WorkPriority::High, work).unwrap(),
            QueueWorkResult::Queued
        );
    });
    assert_no_alloc_or_free(|| {
        assert_eq!(
            queue
                .service_batch(0, WorkPriority::High)
                .unwrap()
                .executed(),
            1
        );
    });

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::High, work).unwrap(),
        QueueWorkResult::Queued
    );
    let cancellation = assert_no_alloc_or_free(|| queue.begin_cancel(work));
    assert_no_alloc_or_free(|| {
        assert_eq!(
            queue
                .service_batch(0, WorkPriority::High)
                .unwrap()
                .cancelled(),
            1
        );
    });
    assert!(cancellation.is_complete());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

fn count_callback(data: usize) -> WorkOutcome {
    let calls = unsafe {
        // SAFETY: the test leaks this counter before publishing its address.
        &*ptr::with_exposed_provenance::<AtomicUsize>(data)
    };
    calls.fetch_add(1, Ordering::Relaxed);
    WorkOutcome::Complete
}

fn pinned(work: &'static WorkItem) -> Pin<&'static WorkItem> {
    unsafe {
        // SAFETY: every caller passes a deliberately leaked WorkItem.
        Pin::new_unchecked(work)
    }
}

fn assert_no_alloc_or_free<T>(operation: impl FnOnce() -> T) -> T {
    TRACK_ALLOCATION.with(|tracking| tracking.set(false));
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    let tracking = TrackingScope::begin();
    let output = operation();
    drop(tracking);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(DEALLOCATIONS.load(Ordering::Relaxed), 0);
    output
}

struct TrackingScope;

impl TrackingScope {
    fn begin() -> Self {
        TRACK_ALLOCATION.with(|tracking| tracking.set(true));
        Self
    }
}

impl Drop for TrackingScope {
    fn drop(&mut self) {
        TRACK_ALLOCATION.with(|tracking| tracking.set(false));
    }
}
