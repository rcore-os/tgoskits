// SPDX-License-Identifier: MIT
//! Deferred callbacks that may be queued from interrupt context. The callback
//! itself always runs in task context, on the shared workqueue worker.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::types::IrqWork;

/// Set while the node sits in the dispatch queue.
pub const IRQ_WORK_PENDING: u32 = 0x01;
/// Set while the callback is executing, or about to.
pub const IRQ_WORK_BUSY: u32 = 0x02;
pub const IRQ_WORK_LAZY: u32 = 0x04;
pub const IRQ_WORK_HARD_IRQ: u32 = 0x08;
/// Bits taken together when a node is claimed for queueing.
pub const IRQ_WORK_CLAIMED: u32 = IRQ_WORK_PENDING | IRQ_WORK_BUSY;

/// Types that can designate an [`IrqWork`] slot.
pub trait IrqWorkPtr {
    fn irq_work_ptr(self) -> *mut IrqWork;
}

impl IrqWorkPtr for *mut IrqWork {
    fn irq_work_ptr(self) -> *mut IrqWork {
        self
    }
}

impl IrqWorkPtr for &mut IrqWork {
    fn irq_work_ptr(self) -> *mut IrqWork {
        self
    }
}

impl IrqWorkPtr for &IrqWork {
    fn irq_work_ptr(self) -> *mut IrqWork {
        (self as *const IrqWork).cast_mut()
    }
}

/// Prepares `work` to run `func` when it is dispatched. Clears the node and
/// the callback slot; must not be called while the node is pending.
pub fn init_irq_work<W: IrqWorkPtr>(work: W, func: unsafe extern "C" fn(*mut IrqWork)) {
    let slot = work.irq_work_ptr();
    assert!(!slot.is_null());
    // SAFETY: `slot` is non-null, and the caller owns the storage it points to.
    // The node is idle by contract, so no dispatcher is reading these fields.
    unsafe {
        (*slot).node.next = core::ptr::null_mut();
        (*slot)
            .node
            .flags
            .counter
            .store(0, core::sync::atomic::Ordering::Relaxed);
        (*slot).node.src = 0;
        (*slot).node.dst = 0;
        (*slot).func = Some(func);
        (*slot).irqwait = core::ptr::null_mut();
    }
}

/// View of the node's flag word as an atomic.
///
/// # Safety
///
/// `work` must be non-null and point to a live `IrqWork` for at least `'a`.
#[inline]
unsafe fn flag_word<'a>(work: *mut IrqWork) -> &'a AtomicU32 {
    assert!(!work.is_null());
    // SAFETY: `work` is non-null and live per this function's contract, so
    // taking the address of its flag field is in bounds of that allocation.
    let word = unsafe { core::ptr::addr_of_mut!((*work).node.flags.counter) }.cast::<AtomicU32>();
    // SAFETY: the flag word is 4-byte aligned, lives inside the caller's
    // `IrqWork`, and is only ever accessed atomically through this view.
    unsafe { &*word }
}

/// Tries to take ownership of the node for queueing. Fails if it is already
/// pending. On success the PENDING and BUSY bits are both set.
fn claim(flags: &AtomicU32) -> bool {
    flags
        .try_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
            (bits & IRQ_WORK_PENDING == 0).then_some(bits | IRQ_WORK_CLAIMED)
        })
        .is_ok()
}

/// Queues `work` on the workqueue. Returns false if it is already pending or
/// the queue refused it. Never allocates, so it is safe from hard IRQ context.
pub fn irq_work_queue<W: IrqWorkPtr>(work: W) -> bool {
    let slot = work.irq_work_ptr();
    assert!(!slot.is_null());
    // SAFETY: `slot` is non-null and, per the `init_irq_work` contract, live
    // for as long as it is queued.
    let flags = unsafe { flag_word(slot) };
    if !claim(flags) {
        return false;
    }

    // SAFETY: the claim above gives this call exclusive use of the node until
    // the dispatcher finishes with it; `slot` is live as noted.
    if unsafe { crate::workqueue::queue_irq_work(slot) } {
        return true;
    }

    // The queue did not take the node. Release the claim so the next attempt
    // is not blocked, and wake anyone waiting for the node to go idle.
    flags.fetch_and(!(IRQ_WORK_PENDING | IRQ_WORK_BUSY), Ordering::Release);
    crate::wait::wake_all();
    false
}

/// Runs the callback of a dispatched node, then clears its state bits. Called
/// only from a task-context workqueue worker.
///
/// # Safety
///
/// `work` must be null or a live node that the workqueue has just dequeued
/// and that carries a valid callback in `func`.
pub(crate) unsafe fn run_irq_work(work: *mut IrqWork) {
    if work.is_null() {
        return;
    }
    // SAFETY: `work` is non-null and live, per this function's contract.
    let flags = unsafe { flag_word(work) };
    // SAFETY: the same liveness guarantee covers reading the callback slot.
    let callback = unsafe { (*work).func };
    if let Some(callback) = callback {
        // SAFETY: the callback was installed by `init_irq_work` with a
        // matching `*mut IrqWork` argument contract.
        unsafe { callback(work) };
    }
    flags.fetch_and(!(IRQ_WORK_PENDING | IRQ_WORK_BUSY), Ordering::Release);
    crate::wait::wake_all();
}

/// Blocks until the node is no longer busy, i.e. its callback has returned
/// or it was never queued.
pub fn irq_work_sync<W: IrqWorkPtr>(work: W) {
    let slot = work.irq_work_ptr();
    assert!(!slot.is_null());
    let is_idle = || {
        // SAFETY: `slot` is non-null and kept alive by the caller for the
        // duration of this wait.
        let bits = unsafe { flag_word(slot) }.load(Ordering::Acquire);
        bits & IRQ_WORK_BUSY == 0
    };
    let _ = crate::wait::wait_until_ready(None, &is_idle);
}
