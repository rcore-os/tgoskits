//! Allocation-free raw waker implementation.

use core::task::{RawWaker, RawWakerVTable, Waker};

use super::coroutine::{CoroutineHeader, release_reference, retain_reference, schedule};

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, wake, wake_by_ref, drop_waker);

/// Creates an owning standard waker for one pinned coroutine header.
///
/// # Safety
///
/// `header` must be pinned and live, and the caller must own a reference that
/// keeps it valid until this function retains the waker's independent reference.
pub(super) unsafe fn coroutine_waker(header: *mut CoroutineHeader) -> Waker {
    let header_ref = unsafe {
        // Caller owns a live reference and the pinned header remains valid while
        // the returned Waker owns the reference retained below.
        &*header
    };
    retain_reference(header_ref);
    let raw = RawWaker::new(header.cast(), &VTABLE);
    unsafe {
        // `raw` owns exactly the reference retained above and uses the matching
        // vtable to release it.
        Waker::from_raw(raw)
    }
}

/// Clones one raw-waker reference.
///
/// # Safety
///
/// `data` must originate from this module's vtable and own a live header reference.
unsafe fn clone_waker(data: *const ()) -> RawWaker {
    let header = data.cast_mut().cast::<CoroutineHeader>();
    let header_ref = unsafe {
        // Every call is made through a live RawWaker that owns one reference.
        &*header
    };
    retain_reference(header_ref);
    RawWaker::new(data, &VTABLE)
}

/// Publishes a wake and consumes one raw-waker reference.
///
/// # Safety
///
/// `data` must originate from this module's vtable and own a live header reference.
unsafe fn wake(data: *const ()) {
    let header = data.cast_mut().cast::<CoroutineHeader>();
    unsafe {
        // The consumed RawWaker keeps the header live through publication. The
        // newly queued node takes its own reference before this one is released.
        schedule(header);
        release_reference(header);
    }
}

/// Publishes a wake while retaining the borrowed raw-waker reference.
///
/// # Safety
///
/// `data` must originate from this module's vtable and own a live header reference.
unsafe fn wake_by_ref(data: *const ()) {
    let header = data.cast_mut().cast::<CoroutineHeader>();
    unsafe {
        // The borrowed RawWaker retains its reference after publication returns.
        schedule(header);
    }
}

/// Releases one raw-waker reference through deferred owner reclamation.
///
/// # Safety
///
/// `data` must originate from this module's vtable and own a live header reference.
unsafe fn drop_waker(data: *const ()) {
    let header = data.cast_mut().cast::<CoroutineHeader>();
    unsafe {
        // Releasing the last reference only publishes an intrusive reclaim node;
        // no memory is freed and no user destructor runs in this operation.
        release_reference(header);
    }
}
