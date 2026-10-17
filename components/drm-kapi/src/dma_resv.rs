// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! LinuxKPI reservation objects: Linux 7.2.3 `drivers/dma-buf/dma-resv.c` and
//! `include/linux/dma-resv.h`. Each `dma_resv` keeps its fences in a native
//! sidecar; the iterator and wait helpers read it through `dma_resv_iter_next`.

use alloc::{
    alloc::{alloc, dealloc},
    vec::Vec,
};
use core::{
    ffi::{c_int, c_long, c_void},
    mem::size_of,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    config::{EINVAL, ENOMEM},
    dma_fence::{
        dma_fence_context_alloc, dma_fence_get, dma_fence_is_signaled, dma_fence_put,
        dma_fence_wait_timeout,
    },
    dma_fence_array::dma_fence_array_create_owned,
    types::{DmaFence, DmaResv},
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaResvIter {
    obj: *mut DmaResv,
    usage: u32,
    fence: *mut DmaFence,
    fence_usage: u32,
    index: u32,
    fences: *mut c_void,
    num_fences: u32,
    is_restarted: bool,
}

const _: [(); 48] = [(); size_of::<DmaResvIter>()];

impl DmaResvIter {
    fn new(resv: *mut DmaResv, usage: u32) -> Self {
        Self {
            obj: resv,
            usage,
            fence: ptr::null_mut(),
            fence_usage: 0,
            index: 0,
            fences: ptr::null_mut(),
            num_fences: 0,
            is_restarted: false,
        }
    }
}

#[repr(C)]
struct ReservedFence {
    fence: *mut DmaFence,
    usage: u32,
}

struct ReservationState {
    entries: spin::Mutex<Vec<ReservedFence>>,
}

/// # Safety
///
/// Allocates and initializes `T` on the global heap. The caller is responsible for eventually
/// reclaiming the returned pointer via `free_object`.
unsafe fn alloc_object<T>(value: T) -> *mut T {
    let layout = core::alloc::Layout::new::<T>();
    // SAFETY: `layout` is a valid Layout for `T`.
    let object = unsafe { alloc(layout) }.cast::<T>();
    if object.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `object` was just allocated with sufficient size and alignment for `T`, and is unaliased.
    unsafe { object.write(value) };
    object
}

/// # Safety
///
/// `object` must either be null or point to a live, initialized `T` previously allocated by `alloc_object`.
unsafe fn free_object<T>(object: *mut T) {
    if object.is_null() {
        return;
    }
    // SAFETY: `object` is verified non-null and was allocated with `Layout::new::<T>()` by `alloc_object`.
    unsafe {
        ptr::drop_in_place(object);
        dealloc(object.cast(), core::alloc::Layout::new::<T>());
    }
}

/// # Safety
///
/// If non-null, `resv` must point to an initialized, properly aligned `DmaResv` struct.
unsafe fn reservation_state(
    resv: *mut DmaResv,
    create: bool,
) -> Result<*mut ReservationState, c_int> {
    if resv.is_null() {
        return Err(-EINVAL);
    }
    // SAFETY: `resv` is verified non-null and properly aligned; `fences` is an aligned pointer field.
    let slot = unsafe {
        AtomicPtr::from_ptr(ptr::addr_of_mut!((*resv).fences).cast::<*mut ReservationState>())
    };
    let current = slot.load(Ordering::Acquire);
    if !current.is_null() || !create {
        return Ok(current);
    }
    // SAFETY: Allocates a new `ReservationState` holding an empty vector.
    let state = unsafe {
        alloc_object(ReservationState {
            entries: spin::Mutex::new(Vec::new()),
        })
    };
    if state.is_null() {
        return Err(-ENOMEM);
    }
    match slot.compare_exchange(ptr::null_mut(), state, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Ok(state),
        Err(existing) => {
            // SAFETY: CAS failed so `state` was not published; free the unused allocation.
            unsafe { free_object(state) };
            Ok(existing)
        }
    }
}

/// Reserve capacity in the native reservation list. The backing allocation is
/// real and all later fence insertion paths retain a reference.
///
/// # Safety
/// `resv` must point to a live reservation object that is not concurrently
/// finalized or reinitialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_reserve_fences(resv: *mut DmaResv, count: u32) -> c_int {
    // SAFETY: `resv` is live per function safety contract.
    let state = match unsafe { reservation_state(resv, true) } {
        Ok(state) => state,
        Err(error) => return error,
    };
    // SAFETY: `state` is a live, valid `ReservationState` pointer.
    match unsafe { (*state).entries.lock().try_reserve(count as usize) } {
        Ok(()) => 0,
        Err(_) => -ENOMEM,
    }
}

/// Linux `dma_resv_add_fence()`.
///
/// # Safety
/// `resv` must point to a live reservation object and `fence` to a live fence
/// reference; the reservation retains an additional reference to the fence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_add_fence(resv: *mut DmaResv, fence: *mut DmaFence, usage: u32) {
    // SAFETY: `resv` and `fence` satisfy caller preconditions forwarded to `dma_resv_add_fence_native`.
    unsafe { dma_resv_add_fence_native(resv, fence, usage) };
}

/// Insert a fence into the reservation and retain its ownership reference.
///
/// # Safety
/// `resv` must point to a live reservation object and `fence` to a live fence
/// reference; the reservation retains an additional reference to the fence.
pub unsafe fn dma_resv_add_fence_native(resv: *mut DmaResv, fence: *mut DmaFence, usage: u32) {
    assert!(!fence.is_null(), "dma_resv_add_fence received a null fence");
    // SAFETY: `resv` is a live reservation per safety contract.
    let state = unsafe { reservation_state(resv, true) }
        .unwrap_or_else(|error| panic!("dma_resv_add_fence failed: {error}"));
    // SAFETY: `state` is a valid `ReservationState` pointer.
    let mut entries = unsafe { (*state).entries.lock() };
    if entries.len() == entries.capacity() {
        entries
            .try_reserve(1)
            .expect("dma_resv_add_fence requires successful prior reservation");
    }
    // SAFETY: the caller passes a live fence; the table takes its own reference.
    unsafe { dma_fence_get(fence) };
    entries.push(ReservedFence { fence, usage });
    drop(entries);
}

/// Iterate reservation fences with Linux's usage ordering and a live cursor
/// reference, released by the caller after iteration.
///
/// # Safety
/// `cursor` must point to a live initialized iterator whose `obj` is a live
/// reservation; it must not be used concurrently or after the reservation is
/// finalized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_iter_next(cursor: *mut DmaResvIter) -> *mut DmaFence {
    if cursor.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `cursor` is non-null and its `obj` field points to a live reservation per contract.
    let state = match unsafe { reservation_state((*cursor).obj, false) } {
        Ok(state) if !state.is_null() => state,
        _ => return ptr::null_mut(),
    };
    // SAFETY: `state` is valid and live.
    let entries = unsafe { (*state).entries.lock() };
    // SAFETY: `cursor` is non-null and points to a live `DmaResvIter`.
    let usage = unsafe { (*cursor).usage };
    // SAFETY: `cursor` is non-null and points to a live `DmaResvIter`.
    let start = unsafe { (*cursor).index as usize };
    let next = entries
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, entry)| entry.usage <= usage);
    let Some((index, entry)) = next else {
        return ptr::null_mut();
    };
    // SAFETY: table entries hold a reference, so `entry.fence` is live.
    let new_fence = unsafe { dma_fence_get(entry.fence) };
    // SAFETY: `cursor` is non-null and points to a live `DmaResvIter`.
    let old_fence = unsafe { (*cursor).fence };
    if !old_fence.is_null() {
        // SAFETY: `old_fence` was previously retained by cursor; release the old reference.
        unsafe { dma_fence_put(old_fence) };
    }
    // SAFETY: `cursor` is non-null and writable per contract; updates iteration state.
    unsafe {
        (*cursor).fence = new_fence;
        (*cursor).fence_usage = entry.usage;
        (*cursor).index = index.saturating_add(1) as u32;
        (*cursor).fences = state.cast();
        (*cursor).num_fences = entries.len() as u32;
        (*cursor).is_restarted = false;
    }
    new_fence
}

/// Tear down the reservation sidecar and release all retained fence refs.
///
/// # Safety
/// `resv` must point to a live reservation that has no concurrent users or
/// active iterators; it must not be finalized more than once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_fini(resv: *mut DmaResv) {
    if resv.is_null() {
        return;
    }
    // SAFETY: `resv` is non-null and valid; atomically unlinks the `ReservationState` sidecar.
    let state = unsafe {
        AtomicPtr::from_ptr(ptr::addr_of_mut!((*resv).fences).cast::<*mut ReservationState>())
            .swap(ptr::null_mut(), Ordering::AcqRel)
    };
    if state.is_null() {
        return;
    }
    // SAFETY: `state` was successfully unlinked and is exclusively owned; drops all stored fence references.
    for entry in unsafe { (*state).entries.lock() }.iter() {
        // SAFETY: `entry.fence` is an owned reference stored in the reservation.
        unsafe { dma_fence_put(entry.fence) };
    }
    // SAFETY: `state` is exclusively owned and now empty; deallocates the sidecar.
    unsafe { free_object(state) };
}

/// Initialize a reservation sidecar when a framework owner initializes its
/// `dma_resv` record, including its native WW mutex.
///
/// # Safety
/// `resv` must point to writable, properly aligned reservation storage with
/// no concurrent users or previously retained fence references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_init(resv: *mut DmaResv) {
    if resv.is_null() {
        return;
    }
    // SAFETY: `resv` is verified non-null and writable per contract; initializes the embedded mutex.
    unsafe {
        crate::mutex::mutex_init(ptr::addr_of_mut!((*resv).lock.base));
        (*resv).lock.ctx = ptr::null_mut();
    }
    // SAFETY: `resv` is non-null and properly aligned; initializes fences slot to null.
    let slot = unsafe {
        AtomicPtr::from_ptr(ptr::addr_of_mut!((*resv).fences).cast::<*mut ReservationState>())
    };
    slot.store(ptr::null_mut(), Ordering::Release);
}

/// Release the reference the reservation cursor holds on its current fence.
///
/// # Safety
///
/// `cursor.fence` must either be null or an owned reference to a live `DmaFence`.
unsafe fn resv_iter_end(cursor: &mut DmaResvIter) {
    if !cursor.fence.is_null() {
        // SAFETY: `cursor.fence` is non-null and owned per safety contract.
        unsafe { dma_fence_put(cursor.fence) };
        cursor.fence = core::ptr::null_mut();
    }
}

/// Linux `dma_resv_test_signaled()`: true when every fence at or below
/// `usage` has signaled.
///
/// # Safety
/// `resv` must point to a live initialized reservation that is not concurrently
/// finalized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_test_signaled(resv: *mut c_void, usage: c_int) -> bool {
    assert!(!resv.is_null());
    let mut cursor = DmaResvIter::new(resv.cast(), usage as u32);
    let mut all = true;
    loop {
        // The cursor keeps the returned fence referenced until the next step.
        // SAFETY: `cursor` is a valid local iterator over the live `resv`.
        let fence = unsafe { dma_resv_iter_next(&mut cursor) };
        if fence.is_null() {
            break;
        }
        // SAFETY: `fence` is a live, valid fence returned and referenced by the cursor.
        if !unsafe { dma_fence_is_signaled(fence) } {
            all = false;
            break;
        }
    }
    // SAFETY: `cursor` holds at most one fence reference that needs release at end of iteration.
    unsafe { resv_iter_end(&mut cursor) };
    all
}

/// Linux `dma_resv_wait_timeout()`: wait for every fence at or below `usage`.
/// Returns the remaining jiffies (at least 1), or 0 when the wait timed out.
///
/// # Safety
/// `resv` must point to a live initialized reservation that remains valid for
/// the duration of the wait and is not concurrently finalized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_wait_timeout(
    resv: *mut c_void,
    usage: c_int,
    intr: bool,
    timeout: c_long,
) -> c_long {
    assert!(!resv.is_null());
    let mut cursor = DmaResvIter::new(resv.cast(), usage as u32);
    let mut remaining = timeout;
    loop {
        // SAFETY: `cursor` is a valid local iterator over the live `resv`.
        let fence = unsafe { dma_resv_iter_next(&mut cursor) };
        if fence.is_null() {
            break;
        }
        // SAFETY: `fence` is live and referenced by `cursor`.
        let left = unsafe { dma_fence_wait_timeout(fence, intr, remaining) };
        if left == 0 {
            // SAFETY: `cursor` holds a live fence reference that must be released before returning.
            unsafe { resv_iter_end(&mut cursor) };
            return 0;
        }
        remaining = left;
    }
    // SAFETY: Releases the last fence reference held by the cursor.
    unsafe { resv_iter_end(&mut cursor) };
    if remaining != 0 { remaining } else { 1 }
}

/// Linux `dma_resv_get_singleton()`: one fence that stands for every fence at
/// or below `usage`. No fence gives NULL, one fence is returned by reference,
/// and several are gathered into a fence array that signals when all do.
///
/// # Safety
/// `resv` must point to a live initialized reservation, and `fence_out` to
/// writable storage for one owned fence reference. The reservation must not
/// be concurrently finalized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_resv_get_singleton(
    resv: *mut c_void,
    usage: c_int,
    fence_out: *mut *mut DmaFence,
) -> c_int {
    assert!(!resv.is_null() && !fence_out.is_null());
    let mut cursor = DmaResvIter::new(resv.cast(), usage as u32);
    // Every gathered fence takes its own reference; the cursor's reference is
    // released once iteration ends.
    let mut fences: alloc::vec::Vec<*mut DmaFence> = alloc::vec::Vec::new();
    loop {
        // SAFETY: `cursor` is a valid local iterator over the live `resv`.
        let fence = unsafe { dma_resv_iter_next(&mut cursor) };
        if fence.is_null() {
            break;
        }
        // SAFETY: `fence` came from the iterator, which holds a reference.
        fences.push(unsafe { dma_fence_get(fence) });
    }
    // SAFETY: Releases the fence reference held by the cursor.
    unsafe { resv_iter_end(&mut cursor) };
    let result = match fences.len() {
        0 => core::ptr::null_mut(),
        1 => fences[0],
        // SAFETY: `fences` contains owned fence references gathered above; `dma_fence_array_create_owned`
        // takes ownership and returns an array fence.
        _ => unsafe {
            dma_fence_array_create_owned(fences, dma_fence_context_alloc(1), 1).cast::<DmaFence>()
        },
    };
    // SAFETY: `fence_out` is verified non-null and writable per contract.
    unsafe { *fence_out = result };
    0
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{
        config::ENOENT,
        dma_fence::{
            dma_fence_add_callback, dma_fence_init, dma_fence_is_signaled, dma_fence_signal,
        },
        locks::spin_lock_init,
        types::{DmaFenceCb, DmaFenceOps, Spinlock},
    };
    const DMA_RESV_USAGE_KERNEL: u32 = 0;
    static CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    ///
    /// Callback invoked by the fence signaling infrastructure.
    unsafe extern "C" fn completed(_: *mut DmaFence, _: *mut DmaFenceCb) {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn fence_callbacks_and_reservation_references_have_real_lifetimes() {
        // SAFETY: Test operates on stack-allocated fences, locks, and reservations initialized
        // and torn down within this test function.
        unsafe {
            let mut fence: DmaFence = core::mem::zeroed();
            let mut lock: Spinlock = core::mem::zeroed();
            let ops: DmaFenceOps = core::mem::zeroed();
            spin_lock_init(&mut lock);
            dma_fence_init(&mut fence, &ops, &mut lock, 1, 2);
            let mut callback: DmaFenceCb = core::mem::zeroed();
            assert_eq!(
                dma_fence_add_callback(&mut fence, &mut callback, completed),
                0
            );
            assert!(!dma_fence_is_signaled(&mut fence));
            dma_fence_signal(&mut fence);
            dma_fence_signal(&mut fence);
            assert!(dma_fence_is_signaled(&mut fence));
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            assert_eq!(
                dma_fence_add_callback(&mut fence, &mut callback, completed),
                -ENOENT
            );
            // Also exercises the empty callback list and timestamp union switch.
            let mut empty: DmaFence = core::mem::zeroed();
            dma_fence_init(&mut empty, &ops, &mut lock, 1, 3);
            dma_fence_signal(&mut empty);
            let mut resv: DmaResv = core::mem::zeroed();
            dma_resv_init(&mut resv);
            assert_eq!(dma_resv_reserve_fences(&mut resv, 1), 0);
            dma_resv_add_fence_native(&mut resv, &mut fence, DMA_RESV_USAGE_KERNEL);
            let mut cursor: DmaResvIter = core::mem::zeroed();
            cursor.obj = &mut resv;
            cursor.usage = DMA_RESV_USAGE_KERNEL;
            assert_eq!(dma_resv_iter_next(&mut cursor), &mut fence as *mut _);
            assert!(dma_resv_iter_next(&mut cursor).is_null());
            dma_fence_put(cursor.fence);
            dma_resv_fini(&mut resv);
            assert_eq!(fence.refcount.refcount.refs.counter, 1);
        }
    }

    #[test]
    fn singleton_multi_fence_array_owns_kernel_allocated_member_buffer() {
        // SAFETY: Test exercises `dma_resv_get_singleton` with stack-allocated and initialized test fences.
        unsafe {
            let mut lock: Spinlock = core::mem::zeroed();
            spin_lock_init(&mut lock);
            let ops: DmaFenceOps = core::mem::zeroed();
            let mut first: DmaFence = core::mem::zeroed();
            let mut second: DmaFence = core::mem::zeroed();
            dma_fence_init(&mut first, &ops, &mut lock, 10, 1);
            dma_fence_init(&mut second, &ops, &mut lock, 10, 2);

            let mut resv: DmaResv = core::mem::zeroed();
            dma_resv_init(&mut resv);
            dma_resv_add_fence_native(&mut resv, &mut first, DMA_RESV_USAGE_KERNEL);
            dma_resv_add_fence_native(&mut resv, &mut second, DMA_RESV_USAGE_KERNEL);

            let mut singleton = core::ptr::null_mut();
            assert_eq!(
                dma_resv_get_singleton(
                    (&mut resv as *mut DmaResv).cast(),
                    DMA_RESV_USAGE_KERNEL as c_int,
                    &mut singleton,
                ),
                0
            );
            assert!(!singleton.is_null());
            assert_eq!(
                (*singleton).ops,
                core::ptr::addr_of!(crate::dma_fence_array::dma_fence_array_ops).cast()
            );

            // The array release synchronously kfrees its member buffer. This
            // used to panic because the reservation path passed a Vec buffer.
            dma_fence_put(singleton);
            dma_resv_fini(&mut resv);
            assert_eq!(first.refcount.refcount.refs.counter, 1);
            assert_eq!(second.refcount.refcount.refs.counter, 1);
            crate::rcu::rcu_barrier();
        }
    }
}
