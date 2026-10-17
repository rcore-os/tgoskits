// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_vblank_work.c`.
//!
//! Vblank-scheduled work: work items queued on a CRTC's kthread worker that
//! run once the vblank counter reaches a target count. Functions are named
//! and ordered as in the C file.

// The Linux ports state the invariant of each C-ABI call once in the module
// header, as the GT ports do; per-block SAFETY prose is not repeated.
#![allow(non_snake_case)]

use alloc::string::String;
use core::fmt::Write as _;

use drm_kapi::{
    kthread::{
        self, KthreadWork, cancel_work_sync, flush_work, flush_worker, queue_work, run_worker,
    },
    list::{INIT_LIST_HEAD, list_add_tail, list_del_init, list_empty},
    locks::{
        spin_lock_irq_raw, spin_lock_irqsave_raw, spin_lock_raw, spin_unlock_irq_raw,
        spin_unlock_irqrestore_raw, spin_unlock_raw,
    },
    types::ListHead,
    wait::{init_waitqueue_head, wake_up_all},
};

use crate::{
    drm_crtc_types::DrmCrtc, drm_device_types::DrmDevice, drm_vblank_types::DrmVblankCrtc,
    drm_vblank_upstream::drm_crtc_vblank_crtc, drm_vblank_work_types::DrmVblankWork,
};

/// Drops the vblank reference and queues every pending work whose target
/// count has been reached. Called with `dev->event_lock` held.
// upstream: drm_vblank_work.c drm_handle_vblank_works()
/// # Safety
/// - `vblank` must be non-null, properly aligned, and point to a valid, initialized `DrmVblankCrtc`.
/// - The caller must hold `(*(*vblank).dev).event_lock` with interrupts disabled.
/// - Every node linked in `(*vblank).pending_work` must be embedded within a live, allocated
///   `DrmVblankWork` at offset `offset_of!(DrmVblankWork, node)`.
/// - `(*vblank).worker` must point to an active `kthread_worker` ready to accept queued work items.
pub unsafe fn drm_handle_vblank_works(vblank: *mut DrmVblankCrtc) {
    // SAFETY: `vblank` is non-null and valid, and caller holds `dev->event_lock` exclusively.
    // All nodes in `pending_work` are valid `ListHead` instances embedded in live `DrmVblankWork` items.
    // Detaching qualified works via `list_del_init`, dropping vblank refs via `drm_vblank_put`,
    // queuing works on `worker`, and waking `work_wait_queue` are all performed under lock protection.
    unsafe {
        let count = (*vblank).count.counter as u64;
        let mut wake = false;

        let dev = (*vblank).dev;
        let head: *mut ListHead = core::ptr::addr_of_mut!((*vblank).pending_work);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let work = (cur as *mut u8).sub(core::mem::offset_of!(DrmVblankWork, node))
                as *mut DrmVblankWork;
            if !crate::drm_vblank_upstream::drm_vblank_passed(count, (*work).count) {
                cur = next;
                continue;
            }

            list_del_init(core::ptr::addr_of_mut!((*work).node));
            crate::drm_vblank_upstream::drm_vblank_put(dev, (*vblank).pipe);
            queue_work(
                (*vblank).worker.cast(),
                core::ptr::addr_of_mut!((*work).base),
            );
            wake = true;
            cur = next;
        }

        if wake {
            wake_up_all(&mut (*vblank).work_wait_queue);
        }
    }
}

/// Removes all pending works without running them. Called with
/// `dev->event_lock` held.
// upstream: drm_vblank_work.c drm_vblank_cancel_pending_works()
/// # Safety
/// - `vblank` must be non-null, properly aligned, and point to an initialized `DrmVblankCrtc`.
/// - The caller must hold `(*(*vblank).dev).event_lock`.
/// - Every node in `(*vblank).pending_work` must be embedded within a live `DrmVblankWork` at
///   `offset_of!(DrmVblankWork, node)`.
pub unsafe fn drm_vblank_cancel_pending_works(vblank: *mut DrmVblankCrtc) {
    // SAFETY: `vblank` is valid and caller holds `dev->event_lock`. Iterating through `pending_work`,
    // computing `DrmVblankWork` container addresses, unlinking each node via `list_del_init`,
    // dropping each vblank ref via `drm_vblank_put`, and waking waiters on `work_wait_queue` are all
    // synchronized by `event_lock`.
    unsafe {
        let dev = (*vblank).dev;
        let head: *mut ListHead = core::ptr::addr_of_mut!((*vblank).pending_work);

        // drm_WARN_ONCE(dev, !list_empty(&vblank->pending_work), "Cancelling pending vblank works!\n")
        drm_kapi::assert::warn_on(!list_empty(&*head));

        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let work = (cur as *mut u8).sub(core::mem::offset_of!(DrmVblankWork, node))
                as *mut DrmVblankWork;
            list_del_init(core::ptr::addr_of_mut!((*work).node));
            crate::drm_vblank_upstream::drm_vblank_put(dev, (*vblank).pipe);
            cur = next;
        }

        wake_up_all(&mut (*vblank).work_wait_queue);
    }
}

/// Schedules `work` to run once the vblank count reaches `count`.
/// Returns a negative errno on failure, otherwise nonzero when scheduled.
// upstream: drm_vblank_work.c drm_vblank_work_schedule()
#[unsafe(no_mangle)]
/// # Safety
/// - `work` must be non-null, properly aligned, and point to an initialized `DrmVblankWork`.
/// - `(*work).vblank` must point to a live `DrmVblankCrtc` associated with an initialized `DrmDevice`.
/// - `work` and its containing CRTC must remain valid and not be freed while the work is pending or executing.
pub unsafe extern "C" fn drm_vblank_work_schedule(
    work: *mut DrmVblankWork,
    count: u64,
    nextonmiss: bool,
) -> i32 {
    // SAFETY: `work` and `(*work).vblank` are valid, initialized objects. Acquiring `event_lock` with
    // `spin_lock_irqsave_raw` and `vbl_lock` with `spin_lock_raw` ensures mutual exclusion while reading
    // `cancelling`/`inmodeset` and updating the work's target count and list membership. Calling
    // `drm_vblank_get`/`drm_vblank_put` and `queue_work` are valid under their respective DRM/kthread contracts.
    unsafe {
        let vblank = (*work).vblank;
        let dev = (*vblank).dev;
        let mut irqflags: core::ffi::c_ulong = 0;
        let mut rescheduling = false;
        let mut wake = false;
        let mut ret: i32 = 0;

        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut irqflags);
        'out: {
            if (*work).cancelling != 0 {
                break 'out;
            }

            spin_lock_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));
            let inmodeset = (*vblank).inmodeset;
            spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));
            if inmodeset != 0 {
                break 'out;
            }

            if list_empty(&(*work).node) {
                ret = crate::drm_vblank_upstream::drm_vblank_get(dev, (*vblank).pipe);
                if ret < 0 {
                    break 'out;
                }
            } else if (*work).count == count {
                // Already scheduled w/ same vbl count
                break 'out;
            } else {
                rescheduling = true;
            }

            (*work).count = count;
            let cur_vbl = crate::drm_vblank_upstream::drm_vblank_count(dev, (*vblank).pipe);
            let passed = crate::drm_vblank_upstream::drm_vblank_passed(cur_vbl, count);
            // drm_dbg_core(): debug print, dropped.

            if !nextonmiss && passed {
                crate::drm_vblank_upstream::drm_vblank_put(dev, (*vblank).pipe);
                ret = queue_work(
                    (*vblank).worker.cast(),
                    core::ptr::addr_of_mut!((*work).base),
                ) as i32;
                if rescheduling {
                    list_del_init(core::ptr::addr_of_mut!((*work).node));
                    wake = true;
                }
            } else {
                if !rescheduling {
                    list_add_tail(
                        core::ptr::addr_of_mut!((*work).node),
                        core::ptr::addr_of_mut!((*vblank).pending_work),
                    );
                }
                ret = 1; // true
            }
        }
        // out:
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), irqflags);
        if wake {
            wake_up_all(&mut (*vblank).work_wait_queue);
        }
        ret
    }
}

/// Cancels a vblank work item, waiting for a running instance to finish.
/// Returns true if the work was pending or running.
// upstream: drm_vblank_work.c drm_vblank_work_cancel_sync()
#[unsafe(no_mangle)]
/// # Safety
/// - `work` must be non-null, properly aligned, and point to an initialized `DrmVblankWork`.
/// - `(*work).vblank` and `(*(*work).vblank).dev` must point to valid, live objects.
/// - Must not be called from interrupt context or while holding locks that the work item's handler
///   might acquire, as `cancel_work_sync` blocks until execution completes.
pub unsafe extern "C" fn drm_vblank_work_cancel_sync(work: *mut DrmVblankWork) -> bool {
    // SAFETY: `work`, `vblank`, and `dev` are valid per caller contract. `event_lock` protects inspecting
    // and unlinking `(*work).node`, decrementing the vblank ref, and maintaining `(*work).cancelling`.
    // Calling `cancel_work_sync` on the embedded `KthreadWork` blocks until any executing instance finishes.
    unsafe {
        let vblank = (*work).vblank;
        let dev = (*vblank).dev;
        let mut ret = false;

        spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        if !list_empty(&(*work).node) {
            list_del_init(core::ptr::addr_of_mut!((*work).node));
            crate::drm_vblank_upstream::drm_vblank_put(vblank_dev(vblank), (*vblank).pipe);
            ret = true;
        }
        (*work).cancelling += 1;
        spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        wake_up_all(&mut (*vblank).work_wait_queue);

        if cancel_work_sync(core::ptr::addr_of_mut!((*work).base)) {
            ret = true;
        }

        spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        (*work).cancelling -= 1;
        spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        ret
    }
}

/// # Safety
/// - `vblank` must be non-null, properly aligned, and point to an initialized `DrmVblankCrtc`.
/// - `vblank` must remain live for the duration of the call.
#[inline]
unsafe fn vblank_dev(vblank: *mut DrmVblankCrtc) -> *mut DrmDevice {
    // SAFETY: `vblank` is verified valid, aligned, and live per caller preconditions;
    // reading field `dev` performs an aligned, non-overlapping read of the stored `DrmDevice` pointer.
    unsafe { (*vblank).dev }
}

/// Waits until the work is no longer pending, then flushes the kthread work.
// upstream: drm_vblank_work.c drm_vblank_work_flush()
#[unsafe(no_mangle)]
/// # Safety
/// - `work` must be non-null, properly aligned, and point to an initialized `DrmVblankWork`.
/// - `(*work).vblank` and its corresponding `dev` must remain live throughout the wait.
/// - Must be called from a sleepable process context, because `cond_resched` and `flush_work` can sleep.
pub unsafe extern "C" fn drm_vblank_work_flush(work: *mut DrmVblankWork) {
    // SAFETY: `work`, `vblank`, and `dev` are valid and live. `event_lock` is repeatedly acquired and
    // released to check `(*work).node` list membership while yielding CPU via `cond_resched`.
    // Once unlinked from pending list, `flush_work` flushes the embedded `base: KthreadWork`.
    unsafe {
        let vblank = (*work).vblank;
        let dev = (*vblank).dev;

        spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        // wait_event_lock_irq(vblank->work_wait_queue, list_empty(&work->node), dev->event_lock)
        while !list_empty(&(*work).node) {
            spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_kapi::wait::cond_resched();
            spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        }
        spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        flush_work(core::ptr::addr_of_mut!((*work).base));
    }
}

/// Waits until no vblank work is pending on the CRTC, then flushes its worker.
// upstream: drm_vblank_work.c drm_vblank_work_flush_all()
#[unsafe(no_mangle)]
/// # Safety
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// - The CRTC must have initialized vblank state so that `drm_crtc_vblank_crtc(crtc)` returns
///   a non-null, valid `DrmVblankCrtc`.
/// - Must be called from a sleepable process context, as `cond_resched` and `flush_worker` can block.
pub unsafe extern "C" fn drm_vblank_work_flush_all(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` is valid and points to an initialized CRTC with live `dev` and `vblank` structures.
    // `event_lock` protects polling `(*vblank).pending_work` emptiness across `cond_resched` sleeps.
    // `flush_worker` flushes all queued items on `(*vblank).worker`.
    unsafe {
        let dev = (*crtc).dev;
        let vblank = drm_crtc_vblank_crtc(crtc);

        spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        // wait_event_lock_irq(vblank->work_wait_queue, list_empty(&vblank->pending_work), dev->event_lock)
        while !list_empty(&(*vblank).pending_work) {
            spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_kapi::wait::cond_resched();
            spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        }
        spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        flush_worker((*vblank).worker.cast());
    }
}

/// Initializes a vblank work item for `crtc`, running `func` on the worker.
// upstream: drm_vblank_work.c drm_vblank_work_init()
#[unsafe(no_mangle)]
/// # Safety
/// - `work` must be non-null, properly aligned, and point to unaliased writable memory allocated
///   for a `DrmVblankWork`.
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc` whose vblank
///   subsystem has been initialized.
/// - `func` must be a valid callback function pointer accepting a pointer to `KthreadWork`.
pub unsafe extern "C" fn drm_vblank_work_init(
    work: *mut DrmVblankWork,
    crtc: *mut DrmCrtc,
    func: unsafe extern "C" fn(*mut KthreadWork),
) {
    // SAFETY: `work` points to writable, unaliased memory for `DrmVblankWork`. `kthread::init_work`
    // initializes `(*work).base`, `INIT_LIST_HEAD` initializes `(*work).node`, and `drm_crtc_vblank_crtc`
    // retrieves the valid vblank structure associated with `crtc`.
    unsafe {
        kthread::init_work(core::ptr::addr_of_mut!((*work).base), func);
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*work).node));
        (*work).vblank = drm_crtc_vblank_crtc(crtc);
    }
}

/// Creates the per-CRTC vblank worker thread using SCHED_FIFO.
// upstream: drm_vblank_work.c drm_vblank_worker_init()
/// # Safety
/// - `vblank` must be non-null, properly aligned, and point to an initialized `DrmVblankCrtc`.
/// - `(*vblank).dev` must point to an initialized `DrmDevice`, and `(*(*vblank).dev).primary`
///   must point to an initialized `DrmMinor` with a valid `index`.
/// - Must be called during CRTC vblank setup before any vblank works are queued or scheduled.
pub unsafe fn drm_vblank_worker_init(vblank: *mut DrmVblankCrtc) -> i32 {
    // SAFETY: `vblank` points to valid, writable `DrmVblankCrtc` memory. Initializing `pending_work`
    // and `work_wait_queue` sets up empty synchronization structures. Reading `(*(*(*vblank).dev).primary).index`
    // accesses initialized device fields to name the thread, and storing `worker` into `(*vblank).worker` completes setup.
    unsafe {
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*vblank).pending_work));
        init_waitqueue_head(&mut (*vblank).work_wait_queue);

        let primary_index = (*(*(*vblank).dev).primary).index;
        let mut name = String::new();
        if name.try_reserve_exact(32).is_err() {
            return -drm_kapi::config::ENOMEM;
        }
        if write!(name, "card{}-crtc{}", primary_index, (*vblank).pipe).is_err() {
            return -drm_kapi::config::ENOMEM;
        }
        let worker = match run_worker(name) {
            Ok(worker) => worker,
            Err(error) => return error,
        };
        (*vblank).worker = worker.cast();
        0
    }
}
