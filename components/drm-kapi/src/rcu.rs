// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Preempt-disabled read-side sections and deferred RCU callbacks for the
//! Linux 7.2.3 `call_rcu()` sites. Reclamation callbacks run on a task
//! after the current read-side population quiesces.

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::types::RcuHead;

static RCU_READERS: AtomicUsize = AtomicUsize::new(0);
static RCU_PENDING: AtomicUsize = AtomicUsize::new(0);
static RCU_EPOCH: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn rcu_read_lock() {
    crate::locks::preempt_disable();
    RCU_READERS.fetch_add(1, Ordering::Acquire);
}

#[inline]
pub fn rcu_read_unlock() {
    let old = RCU_READERS.fetch_sub(1, Ordering::Release);
    assert!(old != 0, "unbalanced rcu_read_unlock");
    if old == 1 {
        RCU_EPOCH.fetch_add(1, Ordering::AcqRel);
        crate::wait::wake_all();
    }
    crate::locks::preempt_enable();
}

/// Linux `rcu_access_pointer()` for an RCU-published raw pointer slot. The
/// pointer is read atomically without acquiring the referent; callers must
/// still hold the lifetime protection required by their source path.
///
/// # Safety
/// `slot` must point to a live, properly aligned pointer slot that is accessed
/// atomically by all concurrent readers and writers.
#[inline]
pub unsafe fn rcu_access_pointer<T>(slot: *const *mut T) -> *mut T {
    assert!(!slot.is_null());
    // SAFETY: `slot` is verified non-null by the assertion above and, per the function's
    // safety contract, points to an aligned, live pointer slot suitable for atomic access.
    let atomic = unsafe { AtomicPtr::from_ptr(slot.cast_mut().cast::<*mut T>()) };
    atomic.load(Ordering::Relaxed)
}

/// Linux `rcu_dereference()` pointer load, paired with the release publication
/// in `rcu_assign_pointer!()` and performed inside the caller's RCU read-side
/// critical section.
///
/// # Safety
/// `slot` must point to a live, properly aligned atomically accessed pointer
/// slot, and the caller must hold an RCU read-side lock while using the result.
#[inline]
pub unsafe fn rcu_dereference<T>(slot: *const *mut T) -> *mut T {
    assert!(!slot.is_null());
    // SAFETY: `slot` is verified non-null by the assertion above and, per the function's
    // safety contract, points to an aligned, live pointer slot suitable for atomic access.
    let atomic = unsafe { AtomicPtr::from_ptr(slot.cast_mut().cast::<*mut T>()) };
    atomic.load(Ordering::Acquire)
}

/// Snapshot the current quiescent-state epoch for `cond_synchronize_rcu()`.
#[inline]
pub fn get_state_synchronize_rcu() -> u64 {
    RCU_EPOCH.load(Ordering::Acquire)
}

/// Wait for readers from the observed epoch to leave. The implementation is
/// deliberately conservative: it waits for an empty reader population before
/// advancing the epoch, which is stronger than Linux's queued grace period.
pub fn cond_synchronize_rcu(state: u64) {
    if RCU_EPOCH.load(Ordering::Acquire) != state {
        return;
    }
    let quiescent = || RCU_READERS.load(Ordering::Acquire) == 0;
    let _ = crate::wait::wait_until_ready(None, &quiescent);
    RCU_EPOCH.fetch_add(1, Ordering::AcqRel);
}

/// Header initializer for `struct rcu_head`.
#[inline]
pub fn init_rcu_head(head: &mut RcuHead) {
    head.next = core::ptr::null_mut();
    head.func = None;
}

/// Queue a Linux RCU reclamation callback. If task allocation is temporarily
/// unavailable, retain the object rather than invoke a destructor before a
/// grace period; the caller's reference remains leaked but safe.
pub fn call_rcu(head: *mut RcuHead, callback: unsafe extern "C" fn(*mut RcuHead)) {
    if head.is_null() {
        return;
    }
    RCU_PENDING.fetch_add(1, Ordering::AcqRel);
    let head_address = head as usize;
    let task = crate::task_runtime::spawn(
        alloc::string::String::from("drm-rcu"),
        alloc::boxed::Box::new(move || {
            let quiescent = || RCU_READERS.load(Ordering::Acquire) == 0;
            let _ = crate::wait::wait_until_ready(None, &quiescent);
            // SAFETY: `head_address` originated from non-null `head`, which the caller is obligated
            // to keep valid until reclamation runs. All RCU readers from preceding epochs have drained.
            unsafe { callback(head_address as *mut RcuHead) };
            RCU_PENDING.fetch_sub(1, Ordering::Release);
            crate::wait::wake_all();
        }),
    );
    if task.is_err() {
        // Fail closed: never reclaim without a proven grace period.
        RCU_PENDING.fetch_sub(1, Ordering::Release);
    }
}

/// Cross-crate LinuxKPI entry used by native DRM fence wrappers whose final
/// put may happen from an upstream callback or another non-sleepable context.
///
/// # Safety
/// `head` must point to live `RcuHead` storage that remains valid until the
/// callback runs; `callback` must be a valid callable function for that head.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kapi_call_rcu(
    head: *mut RcuHead,
    callback: Option<unsafe extern "C" fn(*mut RcuHead)>,
) {
    if let Some(callback) = callback {
        call_rcu(head, callback);
    }
}

pub fn rcu_barrier() {
    let drained = || RCU_PENDING.load(Ordering::Acquire) == 0;
    let _ = crate::wait::wait_until_ready(None, &drained);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_count_tracks_nested_sections() {
        let before = RCU_READERS.load(Ordering::Acquire);
        rcu_read_lock();
        rcu_read_lock();
        assert_eq!(RCU_READERS.load(Ordering::Acquire), before + 2);
        rcu_read_unlock();
        rcu_read_unlock();
        assert_eq!(RCU_READERS.load(Ordering::Acquire), before);
    }
}
