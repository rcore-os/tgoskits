use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

use rdif_block::{BlkError, OwnedRequest, RequestFlags, RequestId, RequestOp, SubmitError};

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn returning_an_unaccepted_request_does_not_allocate() {
    let request = OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    };
    let before = ALLOCATION_COUNT.load(Ordering::Relaxed);

    let rejected = SubmitError::new(RequestId::new(7), BlkError::Busy, request);

    let after = ALLOCATION_COUNT.load(Ordering::Relaxed);
    assert_eq!(after, before, "request rejection must not allocate");
    let (id, error, returned) = rejected.into_parts();
    assert_eq!(id, RequestId::new(7));
    assert_eq!(error, BlkError::Busy);
    assert_eq!(returned.op, RequestOp::Flush);
}
