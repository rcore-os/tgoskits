use super::{
    dispatch::{PolicyApplication, PolicyGenerationCommit},
    *,
};
use crate::sched::system::OwnerRqTaskState;

struct IncomingMigrationBatch {
    remote: Arc<CpuRemote>,
    demand: u64,
}

impl IncomingMigrationBatch {
    fn new(remote: Arc<CpuRemote>, demand: u64) -> Self {
        Self { remote, demand }
    }
}

impl Drop for IncomingMigrationBatch {
    fn drop(&mut self) {
        self.remote.release_incoming_migration_demand(self.demand);
    }
}

pub(super) struct OwnerPolicyApply {
    pub(super) commit: PolicyGenerationCommit,
    pub(super) reschedule: Option<RescheduleKind>,
    pub(super) scheduler_deadline_refresh_required: bool,
    pub(super) rt_period_started: bool,
}

mod policy;

mod admission;

mod affinity;

mod control;

#[cfg(all(test, not(miri)))]
mod loom_tests {
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

    fn finish_switch_tail(placement: &AtomicU64, rq_lock: &Mutex<()>) {
        let _rq = rq_lock.lock().unwrap();
        let observed = placement.load(Ordering::Acquire);
        placement.store(observed & !ON_CPU_FLAG, Ordering::Release);
    }

    fn update_policy(placement: &AtomicU64, rq_lock: &Mutex<()>) {
        let _rq = rq_lock.lock().unwrap();
        let classified = placement.load(Ordering::Acquire);
        let outgoing = classified & ON_RQ_MASK == ON_RQ_QUEUED && classified & ON_CPU_FLAG != 0;
        let relink = placement.load(Ordering::Acquire);
        if outgoing && relink & ON_CPU_FLAG == 0 {
            assert_eq!(
                relink & ON_RQ_MASK,
                ON_RQ_NONE,
                "switch tail released on_cpu between policy classification and relink"
            );
        }
    }

    #[test]
    fn policy_relink_cannot_observe_switch_tail_mid_transaction() {
        loom::model(|| {
            let placement = Arc::new(AtomicU64::new(ON_RQ_QUEUED | ON_CPU_FLAG));
            let rq_lock = Arc::new(Mutex::new(()));
            let tail = {
                let placement = Arc::clone(&placement);
                let rq_lock = Arc::clone(&rq_lock);
                thread::spawn(move || finish_switch_tail(&placement, &rq_lock))
            };
            let policy = {
                let placement = Arc::clone(&placement);
                let rq_lock = Arc::clone(&rq_lock);
                thread::spawn(move || update_policy(&placement, &rq_lock))
            };

            tail.join().unwrap();
            policy.join().unwrap();
        });
    }
}
