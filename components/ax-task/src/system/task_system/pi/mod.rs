//! Linux-style per-lock PI waiter ownership and bounded chain propagation.

use super::*;
use crate::{
    PiMutexClaimOutcome, PiMutexLockResult, PiMutexRef, PiMutexWaiters, PiWaitCancelOutcome,
    PiWaitStateError, lock::PreemptScope, lock_pi_mutex_waiters, lock_raw_pi_mutex_waiters,
    try_lock_raw_pi_mutex_waiters,
};

#[derive(Clone, Copy)]
enum PiRqFollowup {
    RemoteReschedule,
    SchedulerWork,
}

struct PiWaiterRefresh {
    owner: Option<ThreadId>,
    owner_next_lock: Option<PiMutexRaw>,
    changed: bool,
    /// The new top waiter of an ownerless lock whose top changed. Linux wakes
    /// this waiter from `rt_mutex_adjust_prio_chain()` step [9]; otherwise no
    /// owner remains to provide the scheduling edge.
    ownerless_wake: Option<Arc<ThreadCore>>,
}

impl TaskSystem {
    /// Acquires a stable task reference without retaining the registry lock.
    ///
    /// This is the local `get_task_struct()` operation used by the PI chain
    /// walk. All PI graph state is protected by the returned task's scheduler
    /// lock, never by the registry lock used for this lookup.
    fn pi_thread_core(&self, thread: ThreadId) -> Result<Arc<ThreadCore>, TaskError> {
        let state = self.state.lock();
        Ok(Arc::clone(&state.thread_record(thread)?.core))
    }

    fn pi_donation(&self, core: &Arc<ThreadCore>) -> Result<PiDonation, TaskError> {
        let (policy, root) = {
            let sched = core.sched().lock();
            (
                core.effective_policy_snapshot(),
                sched.pi.donor.unwrap_or(core.id()),
            )
        };
        self.pi_donation_from_snapshot(core, policy, root)
    }

    fn pi_donation_from_snapshot(
        &self,
        core: &Arc<ThreadCore>,
        policy: SchedulePolicy,
        root: ThreadId,
    ) -> Result<PiDonation, TaskError> {
        let root_core = if root == core.id() {
            Arc::clone(core)
        } else {
            self.pi_thread_core(root)?
        };
        Ok(PiDonation::new(
            policy,
            root,
            core.effective_scheduling_urgency(),
            core,
            &root_core,
        ))
    }
}

mod graph;
mod operations;
mod schedule;
mod transition;

use transition::publish_owner_after_waiter_detach;

#[cfg(axtest)]
mod axtest;
#[cfg(axtest)]
pub use axtest::{
    PiScheduleTestProbeSnapshot, begin_pi_schedule_test_probe, end_pi_schedule_test_probe,
    pi_schedule_test_probe_snapshot,
};
