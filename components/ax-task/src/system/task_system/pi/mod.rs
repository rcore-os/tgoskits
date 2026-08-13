//! Linux-style per-lock PI waiter ownership and bounded chain propagation.

use super::*;
use crate::{
    PiMutexClaimOutcome, PiMutexLockResult, PiMutexRef, PiMutexWaiters, PiTaskId,
    PiWaitCancelOutcome, PiWaitStateError, lock::PreemptScope, lock_pi_mutex_waiters,
    lock_raw_pi_mutex_waiters, try_lock_raw_pi_mutex_waiters,
};

#[derive(Clone, Copy)]
enum PiRqFollowup {
    RemoteReschedule,
    SchedulerWork,
}

struct PiWaiterRefresh {
    owner: Option<ThreadId>,
    changed: bool,
}

#[derive(Clone, Copy)]
struct PiWaiterRekey {
    waiter: ThreadId,
    registration: PiWaitRegistration,
    new_key: PiWaitKey,
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
