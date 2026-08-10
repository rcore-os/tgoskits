//! Linux-style per-lock PI waiter ownership and bounded chain propagation.

use core::{fmt, marker::PhantomData};

use super::*;
use crate::{PiMutexRef, PiMutexWaiters, PiWaitStateError, lock::PreemptScope};

const PI_RELEASE_WAKE_INVARIANT: u32 = 0x5049_574b;

/// Result of entering the PI mutex slow path.
#[must_use = "a registered PI waiter must be blocked, claimed, or cancelled"]
pub enum PiMutexLockResult<'lock> {
    /// A racing fast unlock let this caller acquire the mutex directly.
    Acquired,
    /// The caller is linked in the mutex-owned scheduler waiter tree.
    Waiting(PiWaitToken<'lock>),
}

/// Result of serializing one ownerless PI-mutex claim on its waiter lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiMutexClaimOutcome {
    /// This waiter was still first and became the physical mutex owner.
    Claimed,
    /// The live owner or top waiter changed after the caller's optimistic check.
    Retry,
}

/// Result of trying to cancel one committed PI waiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiWaitCancelOutcome {
    /// The waiter was removed and every inherited donation was withdrawn.
    Cancelled,
    /// Unlock already published an ownerless handoff to this waiter.
    ///
    /// The caller must claim the mutex before observing interruption, matching
    /// Linux `rt_mutex_slowlock_block()` trying the lock before checking the
    /// pending task state.
    HandoffPending,
}

impl fmt::Debug for PiMutexLockResult<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquired => formatter.write_str("Acquired"),
            Self::Waiting(token) => formatter
                .debug_tuple("Waiting")
                .field(&token.thread_id())
                .finish(),
        }
    }
}

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
