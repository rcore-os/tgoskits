//! Loom model for the no-handoff switch-tail `on_cpu` release publication.
//!
//! Mirrors the packed [`SchedulerPlacement`] facts
//! (components/ax-task/src/system/thread_sched/placement.rs) and the owner
//! rq transaction that serializes them. Linux `finish_task_switch()` runs
//! `finish_task(prev)` — the release-store of `prev->on_cpu` — while still
//! holding `rq->lock` and only then calls `finish_lock_switch()`
//! (kernel/sched/core.c). The policy-update thread models
//! `apply_owner_policy_update_locked`, which classifies `Queued { outgoing }`
//! and later re-decides the re-link (`put_prev` vs `activate`) from a fresh
//! `on_cpu` read inside one rq transaction. The model pins the protocol
//! contract: a tail release published outside the owner rq transaction lets
//! the on_cpu flip land between the classification and the re-link read, and
//! the still-queued task is re-linked through `activate()`.

#![cfg(not(miri))]

use loom::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

const ON_RQ_MASK: u64 = 0b11;
const ON_RQ_NONE: u64 = 0;
const ON_RQ_QUEUED: u64 = 1;
const ON_CPU_FLAG: u64 = 1 << 8;

/// The no-handoff switch tail, mirroring `finish_task_switch()`.
fn tail_release(placement: &AtomicU64, rq_lock: &Mutex<()>) {
    // Linux: finish_task() precedes finish_lock_switch(), so the on_cpu
    // release-store is published inside the rq critical section.
    let _rq = rq_lock.lock().unwrap();
    let observed = placement.load(Ordering::Acquire);
    placement.store(observed & !ON_CPU_FLAG, Ordering::Release);
}

/// The owner policy update, mirroring `apply_owner_policy_update_locked`.
fn policy_update(placement: &AtomicU64, rq_lock: &Mutex<()>) {
    let _rq = rq_lock.lock().unwrap();
    let classified = placement.load(Ordering::Acquire);
    let outgoing = classified & ON_RQ_MASK == ON_RQ_QUEUED && classified & ON_CPU_FLAG != 0;
    // reclassify_task() and the class change happen between the two reads;
    // link_owner_ready_thread_locked() then re-reads on_cpu to choose
    // put_prev (claim still held) over activate (claim released).
    let relink = placement.load(Ordering::Acquire);
    if outgoing && relink & ON_CPU_FLAG == 0 {
        // The classifier skipped deactivate() because it saw the outgoing
        // claim; activate() must not run on the still-queued task.
        assert_eq!(
            relink & ON_RQ_MASK,
            ON_RQ_NONE,
            "tail published the on_cpu release between classification and re-link; activate() \
             would hit the still-queued task",
        );
    }
}

#[test]
fn policy_relink_never_observes_tail_release_mid_transaction() {
    loom::model(|| {
        // Queued on its owner rq and still holding the on_cpu stack claim:
        // the legal outgoing window of a no-handoff switch-out.
        let placement = Arc::new(AtomicU64::new(ON_RQ_QUEUED | ON_CPU_FLAG));
        let rq_lock = Arc::new(Mutex::new(()));

        let tail = {
            let placement = Arc::clone(&placement);
            let rq_lock = Arc::clone(&rq_lock);
            thread::spawn(move || tail_release(&placement, &rq_lock))
        };
        let policy = {
            let placement = Arc::clone(&placement);
            let rq_lock = Arc::clone(&rq_lock);
            thread::spawn(move || policy_update(&placement, &rq_lock))
        };

        tail.join().unwrap();
        policy.join().unwrap();
    });
}
