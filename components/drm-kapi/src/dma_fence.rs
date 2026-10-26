// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Fence core behind the `dma_fence_*` C ABI: reference counting, one-shot
//! signaling with completion callbacks, blocking waits, status and timestamp
//! queries, and the shared default ops table. Fence containers live in
//! `dma_fence_array` and `dma_fence_chain`; reservation objects live in
//! `dma_resv`.
//!
//! The symbols are exported once from this module so that drivers and the
//! kernel side link the same definitions.

use core::{
    ffi::{c_char, c_int, c_long, c_ulong, c_void},
    mem::{ManuallyDrop, offset_of},
    ptr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    config::{CONFIG_HZ, EINVAL, ENOENT, ERESTARTSYS, MAX_SCHEDULE_TIMEOUT},
    list::{init_list_head, list_add_tail, list_del_init, list_empty, list_splice_init},
    locks::{spin_lock_irqsave_raw, spin_unlock_irqrestore_raw},
    memory::{
        kfree, kref_get_unless_zero, kref_init, kref_put, kzalloc_obj, refcount_inc_not_zero,
    },
    primitives::ktime_get,
    rcu::{call_rcu, rcu_dereference, rcu_read_lock, rcu_read_unlock},
    types::{
        DmaFence, DmaFenceCb, DmaFenceOps, DmaFenceTimestamp, Kref, ListHead, RcuHead, Spinlock,
    },
    wait::wait_until_ready,
};

/// Bit positions of `DmaFence::flags`, matching the layout drivers test directly.
pub const DMA_FENCE_FLAG_INLINE_LOCK_BIT: u32 = 1;
pub const DMA_FENCE_FLAG_SIGNALED_BIT: u32 = 3;
pub const DMA_FENCE_FLAG_TIMESTAMP_BIT: u32 = 4;
pub const DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT: u32 = 5;
pub const DMA_FENCE_FLAG_USER_BITS: u32 = 6;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Names reported when a fence has no driver-provided ops or name hook.
static DRIVER_NAME_FALLBACK: &[u8] = b"dma-fence\0";
static TIMELINE_NAME_FALLBACK: &[u8] = b"work\0";

/// Wake every thread blocked on a fence condition after a signal transition.
pub fn wake_dma_fence_waiters() {
    crate::wait::wake_all();
}

/// Convert scheduler ticks to nanoseconds at the configured tick rate.
const fn ticks_to_nanos(ticks: u64) -> u64 {
    ticks.saturating_mul(NANOS_PER_SECOND) / CONFIG_HZ as u64
}

/// Convert nanoseconds to whole scheduler ticks at the configured tick rate.
const fn nanos_to_ticks(nanos: u64) -> u64 {
    nanos.saturating_mul(CONFIG_HZ as u64) / NANOS_PER_SECOND
}

/// Mask for one flag bit of `DmaFence::flags`.
const fn flag(bit: u32) -> usize {
    1usize << bit
}

/// Task pointer for the current context, or null when there is none.
fn current_task_ptr() -> *mut c_void {
    match crate::kernel_services::current_task_id().map(usize::try_from) {
        Some(Ok(id)) if id != 0 => id as *mut c_void,
        _ => ptr::null_mut(),
    }
}

/// Whether a signal is pending for `task`. A null task has nothing pending.
///
/// # Safety
/// `task` must be null or a task pointer valid for the scheduler's signal query.
unsafe fn interrupt_pending(task: *mut c_void) -> bool {
    // SAFETY: the null case is answered locally; any other pointer is forwarded
    // to the scheduler query under this function's contract.
    !task.is_null() && unsafe { crate::signal::signal_pending_state(1, task) }
}

/// Atomic view of the flags word of a fence.
///
/// # Safety
/// `fence` must point to a live `DmaFence`. Every access to `flags` made while
/// the returned reference is alive must go through this atomic view.
unsafe fn flag_word<'a>(fence: *mut DmaFence) -> &'a AtomicUsize {
    // SAFETY: `flags` is a naturally aligned word inside a live fence, and the
    // atomic view has the same size and alignment on this x86_64-only target.
    unsafe { AtomicUsize::from_ptr(ptr::addr_of_mut!((*fence).flags).cast()) }
}

/// Acquire-load a flag bit.
///
/// # Safety
/// `fence` must point to a live `DmaFence`.
unsafe fn test_flag(fence: *mut DmaFence, bit: u32) -> bool {
    // SAFETY: forwarded to `flag_word`, which has the same liveness requirement.
    unsafe { flag_word(fence).load(Ordering::Acquire) & flag(bit) != 0 }
}

/// Whether the fence is in the signaled state.
///
/// # Safety
/// `fence` must point to a live `DmaFence`.
pub(crate) unsafe fn is_done_flag(fence: *mut DmaFence) -> bool {
    // SAFETY: forwarded to `test_flag` with the same liveness requirement.
    unsafe { test_flag(fence, DMA_FENCE_FLAG_SIGNALED_BIT) }
}

/// The callback list head, which shares storage with the timestamp until the
/// fence signals.
///
/// # Safety
/// `fence` must point to a live `DmaFence` that has not yet signaled.
unsafe fn callback_list(fence: *mut DmaFence) -> *mut ListHead {
    // SAFETY: the union member is a valid list head while the fence is pending.
    unsafe { ptr::addr_of_mut!((*fence).timestamp_union.cb_list).cast::<ListHead>() }
}

/// The lock protecting a fence: its inline lock when the inline bit is set,
/// otherwise the external lock recorded at init.
///
/// # Safety
/// `fence` must point to a live, initialized `DmaFence`.
unsafe fn lock_of(fence: *mut DmaFence) -> *mut Spinlock {
    // SAFETY: the inline bit decides which union member is valid, and init
    // writes exactly that member before the fence is published.
    if unsafe { test_flag(fence, DMA_FENCE_FLAG_INLINE_LOCK_BIT) } {
        unsafe { ptr::addr_of_mut!((*fence).lock.inline_lock) }
    } else {
        unsafe { (*fence).lock.extern_lock }
    }
}

/// Ops table of a fence, or `None` when the fence has none.
///
/// # Safety
/// `fence` must be null or point to a live `DmaFence`. The returned reference
/// must not outlive the ops table, which the fence's contract keeps alive.
unsafe fn ops_of<'a>(fence: *mut DmaFence) -> Option<&'a DmaFenceOps> {
    if fence.is_null() {
        return None;
    }
    // SAFETY: the fence is live, and `ops` is either null or a table that
    // outlives the fence by contract.
    unsafe { (*fence).ops.cast::<DmaFenceOps>().as_ref() }
}

/// Scope guard that holds a fence's spinlock with IRQ state saved.
struct FenceLockGuard {
    lock: *mut Spinlock,
    saved: c_ulong,
}

impl FenceLockGuard {
    /// Take the fence lock and remember the IRQ state to restore.
    ///
    /// # Safety
    /// `fence` must point to a live, initialized `DmaFence` whose lock stays
    /// valid until the guard is dropped.
    unsafe fn acquire(fence: *mut DmaFence) -> Self {
        // SAFETY: the fence is live per this function's contract.
        let lock = unsafe { lock_of(fence) };
        assert!(!lock.is_null(), "dma fence has no lock");
        let mut saved: c_ulong = 0;
        // SAFETY: `lock` is non-null and valid, and `saved` is local storage.
        unsafe { spin_lock_irqsave_raw(lock, &mut saved) };
        Self { lock, saved }
    }
}

impl Drop for FenceLockGuard {
    fn drop(&mut self) {
        // SAFETY: the guard was created by `acquire`, so this lock is held once
        // by this guard and the saved IRQ state belongs to that acquisition.
        unsafe { spin_unlock_irqrestore_raw(self.lock, self.saved) };
    }
}

/// Take a new reference to a fence the caller already owns.
///
/// Null is passed through unchanged so that null request and fence pointers
/// stay interoperable with the callers.
///
/// # Safety
///
/// `fence` must be null or point to a live fence on which the caller holds a
/// reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_get(fence: *mut DmaFence) -> *mut DmaFence {
    if fence.is_null() {
        return fence;
    }
    // SAFETY: the caller owns a reference, so the fence and its refcount are
    // live memory.
    let took = refcount_inc_not_zero(unsafe { &(*fence).refcount.refcount });
    assert!(took, "dma_fence_get requires an owned live fence reference");
    fence
}

/// Take a reference to a fence found through an RCU-protected pointer.
///
/// Returns null when the last reference was already dropped. The caller must
/// be inside an RCU read-side section.
///
/// # Safety
///
/// `fence` must be null or point to fence memory kept allocated by the
/// caller's RCU read-side section.
pub unsafe fn dma_fence_get_rcu(fence: *mut DmaFence) -> *mut DmaFence {
    if fence.is_null() {
        return fence;
    }
    // SAFETY: the caller is inside an RCU read-side section, so the fence
    // memory stays allocated while its refcount is probed.
    if unsafe { kref_get_unless_zero(&(*fence).refcount) } {
        fence
    } else {
        ptr::null_mut()
    }
}

/// Drop one reference. The final drop runs the driver's release hook, or
/// reclaims the fence after a grace period when the driver has none.
///
/// # Safety
/// `fence` must be null or point to a live fence for which the caller owns one
/// reference. Each owned reference may be put exactly once. The fence's ops
/// table and release callback must remain valid until the final put completes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_put(fence: *mut DmaFence) {
    if fence.is_null() {
        return;
    }
    // SAFETY: one owned reference is being dropped, and `release_final` is the
    // only path that can free the fence.
    unsafe { kref_put(ptr::addr_of_mut!((*fence).refcount), release_final) };
}

/// Recover the fence that embeds `refcount`.
///
/// # Safety
/// `refcount` must point at the `refcount` field of a `DmaFence`.
unsafe fn fence_of_refcount(refcount: *mut Kref) -> *mut DmaFence {
    // SAFETY: the refcount field sits at a fixed offset inside its fence.
    unsafe {
        refcount
            .cast::<u8>()
            .sub(offset_of!(DmaFence, refcount))
            .cast::<DmaFence>()
    }
}

/// Final-reference handler for every fence put path.
///
/// # Safety
/// `refcount` must point at the `refcount` field of a fence whose last
/// reference was just dropped.
unsafe extern "C" fn release_final(refcount: *mut Kref) {
    if refcount.is_null() {
        return;
    }
    // SAFETY: per this function's contract the refcount belongs to a fence.
    let fence = unsafe { fence_of_refcount(refcount) };
    rcu_read_lock();
    // SAFETY: the fence is still allocated, and the ops pointer is read
    // volatilely because a concurrent setter may have just changed it.
    let ops = unsafe { ptr::read_volatile(ptr::addr_of!((*fence).ops)) }.cast::<DmaFenceOps>();
    // SAFETY: a non-null ops pointer names a table that outlives the fence.
    let release = unsafe { ops.as_ref() }.and_then(|table| table.release);
    match release {
        // SAFETY: the driver's release hook takes ownership of the final drop.
        Some(hook) => unsafe { hook(fence) },
        // SAFETY: the final reference is gone, so nobody can reach the fence.
        None => unsafe { defer_reclaim(fence) },
    }
    rcu_read_unlock();
}

/// Free a fence after the current RCU grace period has elapsed.
///
/// # Safety
/// `fence` must point to a live fence whose references are all gone and whose
/// reclamation has not already been scheduled.
unsafe fn defer_reclaim(fence: *mut DmaFence) {
    // SAFETY: the rcu member is part of the fence's timestamp union, so the
    // pointer stays inside the fence allocation.
    let head = unsafe { ptr::addr_of_mut!((*fence).timestamp_union.rcu).cast::<RcuHead>() };
    call_rcu(head, reclaim_after_grace);
}

/// RCU callback that releases the storage of a fence.
///
/// # Safety
/// `head` must be the `timestamp_union` rcu member of a fence whose grace
/// period has elapsed.
unsafe extern "C" fn reclaim_after_grace(head: *mut RcuHead) {
    if head.is_null() {
        return;
    }
    // SAFETY: the head is the rcu member of a fence, at a fixed offset.
    let fence = unsafe {
        head.cast::<u8>()
            .sub(offset_of!(DmaFence, timestamp_union))
            .cast::<DmaFence>()
    };
    // SAFETY: the grace period has passed, so no reader holds this fence.
    unsafe { kfree(fence) };
}

/// Push an absolute monotonic deadline to the fence's driver hook.
///
/// # Safety
/// `fence` must be null or point to a live fence held by the caller. Its ops
/// table, deadline callback, and callback-owned state must remain valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_set_deadline(fence: *mut DmaFence, deadline: i64) {
    // SAFETY: the fence is live per this function's contract.
    if let Some(hook) = unsafe { ops_of(fence) }.and_then(|table| table.set_deadline) {
        // SAFETY: the driver hook is valid for the fence per the contract.
        unsafe { hook(fence, deadline) };
    }
}

/// Return the spinlock protecting a fence.
///
/// # Safety
/// `fence` must point to a live initialized fence whose inline or external
/// lock remains allocated and initialized for the fence's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_spinlock(fence: *mut DmaFence) -> *mut Spinlock {
    assert!(!fence.is_null());
    // SAFETY: the fence is live per this function's contract.
    unsafe { lock_of(fence) }
}

/// Acquire a fence's lock with IRQs disabled, saving the previous IRQ state.
///
/// # Safety
/// `fence` must point to a live initialized fence with a valid lock, and
/// `flags` must point to writable, properly aligned storage. The caller must
/// later unlock this same fence exactly once with the saved flags, on the same
/// execution context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_lock_irqsave(fence: *mut DmaFence, flags: *mut c_ulong) {
    assert!(!fence.is_null() && !flags.is_null());
    // SAFETY: the fence is live and `flags` is writable per the contract.
    unsafe {
        let lock = lock_of(fence);
        spin_lock_irqsave_raw(lock, &mut *flags);
    }
}

/// Release a fence's lock and restore the IRQ state saved when it was taken.
///
/// # Safety
/// `fence` must be a live initialized fence whose lock is currently held by
/// this caller from a matching `dma_fence_lock_irqsave`; `flags` must be the
/// saved interrupt state from that acquisition.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_unlock_irqrestore(fence: *mut DmaFence, flags: c_ulong) {
    assert!(!fence.is_null());
    // SAFETY: the fence is live and its lock is held by the caller per the contract.
    unsafe { spin_unlock_irqrestore_raw(lock_of(fence), flags) };
}

static CONTEXT_IDS: AtomicU64 = AtomicU64::new(1);

/// Reserve `num` consecutive fence context identifiers and return the first.
#[unsafe(no_mangle)]
pub extern "C" fn dma_fence_context_alloc(num: u32) -> u64 {
    CONTEXT_IDS.fetch_add(u64::from(num), Ordering::Relaxed)
}

/// Arm driver signaling for a fence, at most once. If the driver reports that
/// the fence has already completed, the fence is signaled here.
///
/// # Safety
/// `fence` must point to a live initialized fence held by the caller. Its ops
/// table, lock, `enable_signaling` callback, and callback-owned state must stay
/// valid for the call; that callback runs while the fence lock is held and must
/// obey the fence driver's non-reentrancy contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_enable_sw_signaling(fence: *mut DmaFence) {
    assert!(!fence.is_null());
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // SAFETY: the fence is live and its lock is held by `_guard`.
    unsafe { arm_signaling_locked(fence) };
}

/// Set the enable-signal bit and call the driver hook once.
///
/// # Safety
/// `fence` must be a live initialized fence whose lock is held by the caller.
unsafe fn arm_signaling_locked(fence: *mut DmaFence) {
    let bit = flag(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT);
    // SAFETY: the fence is live, and the fetch_or reports the earlier state.
    let before = unsafe { flag_word(fence).fetch_or(bit, Ordering::AcqRel) };
    // SAFETY: the fence is live.
    if before & bit != 0 || unsafe { is_done_flag(fence) } {
        return;
    }
    // SAFETY: the fence is live and its ops table is valid by contract.
    let hook = unsafe { ops_of(fence) }.and_then(|table| table.enable_signaling);
    if let Some(hook) = hook {
        // SAFETY: the driver hook is valid and runs under the fence lock.
        if !unsafe { hook(fence) } {
            // SAFETY: the driver reports nothing left to wait for; the lock is held.
            unsafe { signal_with_lock(fence, None) };
        }
    }
}

/// Detach a callback from its fence if it is still queued.
///
/// Returns true when the callback was queued and is now removed, and false when
/// it had already run or was never queued.
///
/// # Safety
/// `fence` must be a live initialized fence held by the caller. `cb` must be a
/// live callback record whose list node is either self-linked or currently
/// linked on this fence; keep its storage alive until this call returns or the
/// callback has completed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_remove_callback(
    fence: *mut DmaFence,
    cb: *mut DmaFenceCb,
) -> bool {
    assert!(!fence.is_null() && !cb.is_null());
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // SAFETY: `cb` is live per the contract, and its node is valid to inspect
    // under the fence lock.
    unsafe {
        let node = ptr::addr_of_mut!((*cb).node);
        let queued = !list_empty(&*node);
        if queued {
            list_del_init(node);
        }
        queued
    }
}

/// Default blocking wait for fence ops that do not supply their own.
///
/// Returns the remaining ticks (at least 1) once the fence signals. Returns 0
/// on timeout, `-ERESTARTSYS` when an interruptible wait sees a pending signal,
/// and `-EINVAL` for a null fence. A timeout of `MAX_SCHEDULE_TIMEOUT` waits
/// without bound.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence, and the caller
/// must hold a reference to it for the duration of this potentially blocking
/// wait. Do not hold the fence's spinlock while waiting.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_default_wait(
    fence: *mut DmaFence,
    interruptible: bool,
    timeout: c_long,
) -> c_long {
    if fence.is_null() {
        return -(EINVAL as c_long);
    }
    // SAFETY: the fence is live per the contract.
    let done = || unsafe { is_done_flag(fence) };
    if done() {
        return if timeout != 0 { timeout } else { 1 };
    }
    let task = current_task_ptr();
    // SAFETY: `task` is null or the current task, as produced by `current_task_ptr`.
    let interrupted = || interruptible && unsafe { interrupt_pending(task) };
    if interrupted() {
        return -(ERESTARTSYS as c_long);
    }
    if timeout == 0 {
        return 0;
    }

    let started = crate::clock::now_ns();
    let timeout_ns = if timeout == MAX_SCHEDULE_TIMEOUT as c_long {
        None
    } else {
        Some(ticks_to_nanos(timeout.max(0) as u64))
    };
    let ready = || done() || interrupted();
    if !wait_until_ready(timeout_ns, &ready) {
        return 0;
    }
    if !done() {
        return if interrupted() {
            -(ERESTARTSYS as c_long)
        } else {
            0
        };
    }
    if timeout == MAX_SCHEDULE_TIMEOUT as c_long {
        return timeout;
    }
    let spent_ticks = nanos_to_ticks(crate::clock::now_ns().saturating_sub(started));
    timeout.saturating_sub(spent_ticks as c_long).max(1)
}

/// Wait up to `timeout` ticks for a fence to signal.
///
/// Returns the remaining ticks (at least 1) once signaled, or 0 on timeout. A
/// timeout of `MAX_SCHEDULE_TIMEOUT` waits without bound and reports the full
/// budget back on success, because a shortened value of 1 would make a
/// reservation-wait loop give up after a single tick on the next fence.
///
/// # Safety
/// `fence` must point to a live initialized fence and the caller must retain a
/// reference for the duration of the potentially blocking wait. Do not hold
/// its spinlock while waiting; execution must be in a context allowed to block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_wait_timeout(
    fence: *mut DmaFence,
    interruptible: bool,
    timeout: c_long,
) -> c_long {
    assert!(!fence.is_null());
    assert!(
        timeout >= 0,
        "dma_fence_wait_timeout() with negative timeout"
    );
    // SAFETY: the fence is live per the contract.
    if unsafe { dma_fence_is_signaled(fence) } {
        return if timeout != 0 { timeout } else { 1 };
    }
    if timeout == 0 {
        return 0;
    }
    // SAFETY: the fence is live and the caller may block, per the contract.
    unsafe { dma_fence_enable_sw_signaling(fence) };

    // The driver hook and the default implementation share Linux's return
    // contract: positive remaining ticks, zero timeout, negative interruption.
    // SAFETY: The caller retains the live fence, including its static ops table.
    let wait = unsafe { ops_of(fence) }
        .and_then(|ops| ops.wait)
        .unwrap_or(dma_fence_default_wait);
    // SAFETY: Forward the same reference, sleepability and timeout contract.
    unsafe { wait(fence, interruptible, timeout) }
}

/// Initialize a fence in caller-provided storage.
///
/// A null `lock` selects the fence's own inline lock; a non-null `lock` is used
/// as an external lock. The fence starts pending with one reference.
///
/// # Safety
/// `fence` must point to writable, properly aligned, uninitialized `DmaFence`
/// storage that is not yet published. `ops` must point to a stable, valid
/// table that outlives the fence. A non-null `lock` must point to a stable,
/// initialized external lock that outlives the fence; a null `lock` selects
/// and initializes the fence's inline lock. Publish the fence to other threads
/// only after this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_init(
    fence: *mut DmaFence,
    ops: *const DmaFenceOps,
    lock: *mut Spinlock,
    context: u64,
    seqno: u64,
) {
    assert!(!fence.is_null() && !ops.is_null());
    // SAFETY: the storage is writable and uninitialized per the contract, so
    // every field is written with `write` or plain assignment of a Copy value
    // before anything reads it.
    unsafe {
        // The lock field is a union, and the flag records which member is live.
        // Writing the inline lock here (rather than the external pointer) keeps
        // the native spin word initialized.
        if lock.is_null() {
            ptr::addr_of_mut!((*fence).lock.inline_lock).write(Spinlock::new());
            ptr::addr_of_mut!((*fence).flags)
                .write(flag(DMA_FENCE_FLAG_INLINE_LOCK_BIT) as c_ulong);
        } else {
            ptr::addr_of_mut!((*fence).lock.extern_lock).write(lock);
            ptr::addr_of_mut!((*fence).flags).write(0);
        }
        ptr::addr_of_mut!((*fence).ops).write(ops.cast());
        ptr::addr_of_mut!((*fence).timestamp_union).write(DmaFenceTimestamp {
            cb_list: ManuallyDrop::new(ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            }),
        });
        init_list_head(callback_list(fence));
        (*fence).context = context;
        (*fence).seqno = seqno;
        (*fence).error = 0;
        kref_init(ptr::addr_of_mut!((*fence).refcount));
    }
}

/// Record a negative error on a fence. Zero and null are ignored.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence held by the
/// caller. The caller must serialize error updates with signaling/status
/// observation according to the owning fence driver's locking rules.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_set_error(fence: *mut DmaFence, error: c_int) {
    if fence.is_null() || error == 0 {
        return;
    }
    // SAFETY: the fence is live and the caller serializes the update per the contract.
    unsafe { (*fence).error = error };
}

/// Whether a fence has completed, querying the driver's hook when the flag is
/// still clear. A completion seen through the hook is published through the
/// normal signal path, so waiters and callbacks observe it.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence that is not being
/// freed or reinitialized concurrently. Concurrent signaling is permitted.
/// The driver's optional signaled callback and its state must remain valid
/// while this function holds an RCU read-side section.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_is_signaled(fence: *mut DmaFence) -> bool {
    if fence.is_null() {
        return false;
    }
    // SAFETY: the fence is live per the contract.
    if unsafe { is_done_flag(fence) } {
        return true;
    }
    // SAFETY: the fence is live per the contract.
    let done = unsafe { driver_reports_done(fence) };
    if done {
        // SAFETY: the fence is live; signaling takes the lock itself.
        unsafe { dma_fence_signal(fence) };
    }
    done
}

/// Ask the driver's signaled hook whether the fence has completed.
///
/// # Safety
/// `fence` must point to a live initialized fence whose ops table is valid.
unsafe fn driver_reports_done(fence: *mut DmaFence) -> bool {
    rcu_read_lock();
    // SAFETY: the ops slot is read under RCU, and the table it names is valid
    // by the fence's contract for the duration of this section.
    let table = unsafe { rcu_dereference(ptr::addr_of!((*fence).ops).cast::<*mut DmaFenceOps>()) };
    // SAFETY: the table pointer is either null or valid inside the RCU section.
    let done = match unsafe { table.as_ref() }.and_then(|ops| ops.signaled) {
        // SAFETY: the driver hook is valid for this fence by contract.
        Some(hook) => unsafe { hook(fence) },
        None => false,
    };
    rcu_read_unlock();
    done
}

/// Signal a fence while its lock is held. Pending callbacks are detached under
/// the lock, then run in order before this returns.
///
/// # Safety
/// `fence` must be null or a live initialized fence whose lock is held by the
/// caller. Queued callback records and their state must remain live until
/// their callbacks return.
unsafe fn signal_with_lock(fence: *mut DmaFence, timestamp: Option<i64>) {
    if fence.is_null() {
        return;
    }
    // SAFETY: the fence is live and the lock is held per the contract.
    if unsafe { is_done_flag(fence) } {
        return;
    }
    let mut pending = ListHead {
        next: ptr::null_mut(),
        prev: ptr::null_mut(),
    };
    let pending_head: *mut ListHead = &mut pending;
    // SAFETY: the local list head is valid; the callback list is valid while
    // pending, and it is emptied before the timestamp overwrites that storage.
    unsafe {
        init_list_head(pending_head);
        list_splice_init(callback_list(fence), pending_head);
        (*fence).timestamp_union.timestamp = timestamp.unwrap_or_else(ktime_get);
        flag_word(fence).fetch_or(
            flag(DMA_FENCE_FLAG_SIGNALED_BIT) | flag(DMA_FENCE_FLAG_TIMESTAMP_BIT),
            Ordering::Release,
        );
    }
    loop {
        // SAFETY: `pending_head` names the local list, which is only mutated here.
        if unsafe { list_empty(&*pending_head) } {
            break;
        }
        // SAFETY: the list is non-empty, so `next` is a queued callback node.
        let node = unsafe { (*pending_head).next };
        // SAFETY: the node was just found on our local list.
        unsafe { list_del_init(node) };
        let cb = node.cast::<DmaFenceCb>();
        // SAFETY: `cb` is a callback record whose node was queued on this fence.
        if let Some(run) = unsafe { (*cb).func } {
            // SAFETY: the callback's storage is live until it returns, per the
            // contract of `dma_fence_add_callback`.
            unsafe { run(fence, cb) };
        }
    }
    wake_dma_fence_waiters();
}

/// Signal a fence and run its completion callbacks.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence whose lock and
/// ops remain valid. Any queued callback records and their state must remain
/// live until their callbacks return; callbacks run inline before this call
/// finishes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_signal(fence: *mut DmaFence) {
    if fence.is_null() {
        return;
    }
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // SAFETY: the lock is held by `_guard`.
    unsafe { signal_with_lock(fence, None) };
}

/// Signal a fence with an explicit completion timestamp.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence whose lock and
/// operations remain valid. Any queued callback records and their state must
/// remain live until their callbacks return; callbacks run inline before this
/// call finishes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_signal_timestamp(fence: *mut DmaFence, timestamp: i64) {
    if fence.is_null() {
        return;
    }
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // SAFETY: the lock is held by `_guard`.
    unsafe { signal_with_lock(fence, Some(timestamp)) };
}

/// Signal a fence whose lock the caller already holds with IRQ state saved.
///
/// # Safety
/// `fence` must be a live initialized fence, and its fence lock must already
/// be held by the caller with IRQ state saved. Queued callback records and
/// their state must remain live until callbacks, which run inline, return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_signal_locked(fence: *mut DmaFence) {
    // SAFETY: the caller holds the lock per the contract.
    unsafe { signal_with_lock(fence, None) };
}

/// Register a completion callback, or report that the fence already signaled.
///
/// Returns 0 when the callback was queued, `-ENOENT` when the fence had already
/// signaled, and `-EINVAL` for null arguments.
///
/// # Safety
/// `fence` must be a live initialized fence held by the caller. `callback`
/// must point to writable, live callback storage whose node is not linked on
/// any list; retain it and the state used by `function` until it is removed or
/// the callback has completed. `function` must remain valid code for that
/// interval.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_add_callback(
    fence: *mut DmaFence,
    callback: *mut DmaFenceCb,
    function: unsafe extern "C" fn(*mut DmaFence, *mut DmaFenceCb),
) -> c_int {
    if fence.is_null() || callback.is_null() {
        return -EINVAL;
    }
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // Callback registration is itself a request for driver/composite
    // signaling (Linux __dma_fence_enable_signaling). Merely linking the
    // node can leave a poller asleep with no producer armed to notify it.
    // SAFETY: The caller owns writable, unlinked callback storage and the
    // live fence lock is held through arming and list publication.
    unsafe {
        init_list_head(ptr::addr_of_mut!((*callback).node));
        arm_signaling_locked(fence);
    }
    // SAFETY: Arming can synchronously complete the fence under this lock.
    if unsafe { is_done_flag(fence) } {
        return -ENOENT;
    }
    // SAFETY: the callback storage is writable and unlinked per the contract,
    // and the fence's pending list is valid while the lock is held.
    unsafe {
        (*callback).func = Some(function);
        let node = ptr::addr_of_mut!((*callback).node);
        init_list_head(node);
        list_add_tail(node, callback_list(fence));
    }
    0
}

/// Block until a fence signals, or return `-ERESTARTSYS` if an interruptible
/// wait observes a pending task signal. Uninterruptible waits ignore signals.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence, and the caller
/// must hold a reference throughout the wait. The caller must be in a context
/// allowed to block and must not hold the fence spinlock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_wait(fence: *mut DmaFence, intr: bool) -> c_int {
    if fence.is_null() {
        return -EINVAL;
    }
    // SAFETY: The caller retains the fence and may sleep; this uses the same
    // signal predicate and native scheduler wake path as finite waits.
    let result = unsafe { dma_fence_wait_timeout(fence, intr, MAX_SCHEDULE_TIMEOUT as c_long) };
    if result < 0 { result as c_int } else { 0 }
}

/// Driver name of a fence, or a stable fallback when it has none.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence held by the
/// caller. Its ops table and name callback must remain valid; any non-static
/// string returned by the callback must satisfy that callback's lifetime
/// contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_driver_name(fence: *mut DmaFence) -> *const c_char {
    let fallback = DRIVER_NAME_FALLBACK.as_ptr().cast();
    // SAFETY: the fence is live per the contract, or null is handled by `ops_of`.
    match unsafe { ops_of(fence) }.and_then(|table| table.get_driver_name) {
        // SAFETY: the name hook is valid for this fence by contract.
        Some(hook) => unsafe { hook(fence) },
        None => fallback,
    }
}

/// Timeline name of a fence, or a stable fallback when it has none.
///
/// # Safety
/// `fence` must be null or point to a live initialized fence held by the
/// caller. Its ops table and name callback must remain valid, and the returned
/// C string must remain valid according to the callback's lifetime contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_timeline_name(fence: *mut DmaFence) -> *const c_char {
    let fallback = TIMELINE_NAME_FALLBACK.as_ptr().cast();
    // SAFETY: the fence is live per the contract, or null is handled by `ops_of`.
    match unsafe { ops_of(fence) }.and_then(|table| table.get_timeline_name) {
        // SAFETY: the name hook is valid for this fence by contract.
        Some(hook) => unsafe { hook(fence) },
        None => fallback,
    }
}

/// Schedule reclamation of a fence whose references are gone.
///
/// # Safety
/// `fence` must be null or point to a live fence whose reference count has
/// reached zero and whose release path has not already been started. No caller
/// may access it after this call schedules reclamation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_free(fence: *mut DmaFence) {
    if fence.is_null() {
        return;
    }
    // SAFETY: the fence is unreferenced per the contract, so nothing else can
    // reach it while reclamation is scheduled.
    unsafe { defer_reclaim(fence) };
}

/// Status of a fence while its lock is held: 0 while pending, otherwise the
/// recorded error (negative), or 1 when it completed without one. A completion
/// seen through the driver hook is published here, under the lock held by the
/// caller.
///
/// # Safety
/// `fence` must be a live initialized fence whose lock is held by the caller.
unsafe fn status_with_lock(fence: *mut DmaFence) -> c_int {
    // SAFETY: the fence is live and locked per the contract.
    let done = unsafe { is_done_flag(fence) || driver_reports_done(fence) };
    if !done {
        return 0;
    }
    // SAFETY: the fence is live and locked, so the publish is race-free.
    unsafe { signal_with_lock(fence, None) };
    // SAFETY: the fence is live.
    match unsafe { (*fence).error } {
        0 => 1,
        error => error,
    }
}

/// Status of a fence: 0 while pending, the recorded error (negative), or 1 when
/// it completed without one.
///
/// # Safety
/// `fence` must be a live fence.
pub unsafe fn dma_fence_get_status(fence: *mut DmaFence) -> c_int {
    // SAFETY: the fence is live per the contract; the guard releases the lock.
    let _guard = unsafe { FenceLockGuard::acquire(fence) };
    // SAFETY: the lock is held by `_guard`.
    unsafe { status_with_lock(fence) }
}

/// Signal time of a fence. A pending fence reports the current time.
///
/// # Safety
/// `fence` must be a live fence.
pub unsafe fn dma_fence_timestamp(fence: *mut DmaFence) -> i64 {
    // SAFETY: the fence is live per the contract.
    if !unsafe { dma_fence_is_signaled(fence) } {
        return ktime_get();
    }
    // The signaling thread sets the timestamp flag after storing the timestamp,
    // so waiting for that flag makes the stored value visible.
    // SAFETY: the fence is live per the contract.
    while !unsafe { test_flag(fence, DMA_FENCE_FLAG_TIMESTAMP_BIT) } {
        core::hint::spin_loop();
    }
    // SAFETY: the timestamp flag is set, so the union holds the timestamp.
    unsafe { (*fence).timestamp_union.timestamp }
}

/// Whether `f1` was issued after `f2` on the same ops table. Sequence numbers
/// compare with wrap-around. Fences from different ops tables are never ordered.
///
/// # Safety
/// Both pointers must be live fences.
pub unsafe fn dma_fence_is_later(f1: *mut DmaFence, f2: *mut DmaFence) -> bool {
    // SAFETY: both fences are live per the contract.
    let (ops1, seq1, ops2, seq2) = unsafe { ((*f1).ops, (*f1).seqno, (*f2).ops, (*f2).seqno) };
    ops1 == ops2 && (seq1.wrapping_sub(seq2) as i64) > 0
}

unsafe extern "C" fn stub_name(_: *mut DmaFence) -> *const c_char {
    c"Unbound".as_ptr()
}

/// Shared ops table of fences created by `dma_fence_allocate_private_stub`.
static STUB_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(stub_name),
    get_timeline_name: Some(stub_name),
    enable_signaling: None,
    signaled: None,
    wait: None,
    release: None,
    set_deadline: None,
};

/// Allocate a fence that is already signaled at `timestamp` and belongs to no
/// driver. Returns null when allocation fails.
///
/// # Safety
/// Returned fences are released with `dma_fence_put` like any other fence.
pub unsafe fn dma_fence_allocate_private_stub(timestamp: i64) -> *mut DmaFence {
    let fence = kzalloc_obj::<DmaFence>();
    if fence.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the allocation is fresh, zeroed, and not yet shared, so it can be
    // initialized and updated without synchronization.
    unsafe {
        dma_fence_init(fence, ptr::addr_of!(STUB_OPS), ptr::null_mut(), 0, 0);
        (*fence).timestamp_union.timestamp = timestamp;
        flag_word(fence).fetch_or(
            flag(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT)
                | flag(DMA_FENCE_FLAG_SIGNALED_BIT)
                | flag(DMA_FENCE_FLAG_TIMESTAMP_BIT),
            Ordering::Release,
        );
    }
    fence
}

#[cfg(test)]
static PROBE_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static PROBE_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
unsafe extern "C" fn probe_signaled(_: *mut DmaFence) -> bool {
    PROBE_CALLS.fetch_add(1, Ordering::Relaxed);
    PROBE_READY.load(Ordering::Acquire)
}

#[cfg(test)]
static PROBE_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(stub_name),
    get_timeline_name: Some(stub_name),
    enable_signaling: None,
    signaled: Some(probe_signaled),
    wait: None,
    release: None,
    set_deadline: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_timestamp_survives_signal() {
        let mut fence = unsafe { core::mem::zeroed::<DmaFence>() };
        unsafe {
            dma_fence_init(&mut fence, ptr::addr_of!(STUB_OPS), ptr::null_mut(), 0, 1);
            dma_fence_signal_timestamp(&mut fence, 123_456_789);
        }

        assert!(unsafe { dma_fence_is_signaled(&mut fence) });
        assert_eq!(unsafe { dma_fence_timestamp(&mut fence) }, 123_456_789);
    }

    #[test]
    fn null_lock_selects_inline_spinlock() {
        let mut fence = unsafe { core::mem::zeroed::<DmaFence>() };
        unsafe {
            dma_fence_init(&mut fence, ptr::addr_of!(STUB_OPS), ptr::null_mut(), 0, 2);
        }

        assert_ne!(
            fence.flags & (1 << DMA_FENCE_FLAG_INLINE_LOCK_BIT),
            0,
            "a null external lock must select the inline lock"
        );
        let inline_lock = ptr::addr_of_mut!(fence.lock.inline_lock);
        let selected = unsafe { dma_fence_spinlock(&mut fence) };
        assert_eq!(selected, inline_lock);

        let word = unsafe { AtomicUsize::from_ptr(selected.cast::<usize>()) };
        assert_eq!(word.load(Ordering::Acquire), 0);
        let mut flags: c_ulong = 0;
        unsafe { dma_fence_lock_irqsave(&mut fence, &mut flags) };
        assert_eq!(word.load(Ordering::Acquire), 1);
        unsafe { dma_fence_unlock_irqrestore(&mut fence, flags) };
        assert_eq!(word.load(Ordering::Acquire), 0);

        unsafe { dma_fence_signal_timestamp(&mut fence, 123_456_789) };
        assert!(unsafe { dma_fence_is_signaled(&mut fence) });
        assert_eq!(unsafe { dma_fence_timestamp(&mut fence) }, 123_456_789);
    }

    #[test]
    fn driver_hook_is_polled_until_it_reports_completion() {
        PROBE_CALLS.store(0, Ordering::Relaxed);
        PROBE_READY.store(false, Ordering::Release);
        let mut fence = unsafe { core::mem::zeroed::<DmaFence>() };
        unsafe {
            dma_fence_init(&mut fence, ptr::addr_of!(PROBE_OPS), ptr::null_mut(), 0, 3);
        }
        let flags_ptr = ptr::addr_of_mut!(fence.flags).cast::<usize>();
        let flag_at = || unsafe { AtomicUsize::from_ptr(flags_ptr) }.load(Ordering::Acquire);
        let signaled_bit = flag(DMA_FENCE_FLAG_SIGNALED_BIT);
        let timestamp_bit = flag(DMA_FENCE_FLAG_TIMESTAMP_BIT);

        assert!(!unsafe { dma_fence_is_signaled(&mut fence) });
        assert_eq!(PROBE_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(flag_at() & signaled_bit, 0);

        PROBE_READY.store(true, Ordering::Release);
        assert!(unsafe { dma_fence_is_signaled(&mut fence) });
        assert_eq!(PROBE_CALLS.load(Ordering::Relaxed), 2);
        assert_ne!(flag_at() & signaled_bit, 0);
        assert_ne!(flag_at() & timestamp_bit, 0);

        // Once the flag is set the driver hook is no longer consulted.
        assert!(unsafe { dma_fence_is_signaled(&mut fence) });
        assert_eq!(PROBE_CALLS.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn callback_registration_requests_driver_completion_and_handles_already_done() {
        use core::sync::atomic::AtomicBool;

        #[repr(C)]
        struct Probe {
            base: DmaFence,
            pending: AtomicBool,
            requests: AtomicUsize,
            callbacks: AtomicUsize,
        }
        unsafe extern "C" fn enable(fence: *mut DmaFence) -> bool {
            // SAFETY: This ops table belongs only to the live Probe whose
            // first field is the base fence and whose storage outlives it.
            let probe = unsafe { &*fence.cast::<Probe>() };
            probe.requests.fetch_add(1, Ordering::Relaxed);
            probe.pending.load(Ordering::Acquire)
        }
        unsafe extern "C" fn completed(fence: *mut DmaFence, _: *mut DmaFenceCb) {
            // SAFETY: The callback receives the Probe's retained base fence.
            unsafe { &*fence.cast::<Probe>() }
                .callbacks
                .fetch_add(1, Ordering::Release);
        }
        static OPS: DmaFenceOps = DmaFenceOps {
            get_driver_name: None,
            get_timeline_name: None,
            enable_signaling: Some(enable),
            signaled: None,
            wait: None,
            release: None,
            set_deadline: None,
        };
        for pending in [true, false] {
            // SAFETY: dma_fence_init initializes the zero-valid base before use.
            let mut probe = Probe {
                base: unsafe { core::mem::zeroed() },
                pending: AtomicBool::new(pending),
                requests: AtomicUsize::new(0),
                callbacks: AtomicUsize::new(0),
            };
            // SAFETY: Local callback storage remains pinned until removal.
            let mut callback: DmaFenceCb = unsafe { core::mem::zeroed() };
            // SAFETY: Probe, callback and the immutable ops table remain live
            // throughout registration, simulated hardware completion and removal.
            let (result, requested, signaled, calls) = unsafe {
                dma_fence_init(&mut probe.base, &OPS, ptr::null_mut(), 1, 1);
                let result = dma_fence_add_callback(&mut probe.base, &mut callback, completed);
                let requested = probe.requests.load(Ordering::Acquire);
                // The device produces an IRQ only after the callback consumer
                // requested signaling. The fence/callback logic remains real.
                if pending && requested != 0 {
                    probe.pending.store(false, Ordering::Release);
                    dma_fence_signal(&mut probe.base);
                }
                let signaled = dma_fence_is_signaled(&mut probe.base);
                let calls = probe.callbacks.load(Ordering::Acquire);
                dma_fence_remove_callback(&mut probe.base, &mut callback);
                (result, requested, signaled, calls)
            };
            assert_eq!(
                requested, 1,
                "callback registration must arm driver signaling"
            );
            assert!(signaled);
            assert_eq!(result, if pending { 0 } else { -ENOENT });
            assert_eq!(calls, usize::from(pending));
        }
    }
}
