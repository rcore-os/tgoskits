//! Wake consumption, runqueue dispatch, and policy-application internals.

use super::*;
use crate::system::OwnerRqTaskState;

#[derive(Clone, Copy)]
pub(super) struct PolicyGenerationCommit {
    pub(super) base_policy: SchedulePolicy,
    pub(super) application: PolicyApplication,
    pub(super) held_deadline_reservation: u64,
    pub(super) committed_deadline_reservation: u64,
}

#[derive(Clone, Copy)]
pub(super) enum PolicyApplication {
    Current { owner_now_ns: u64 },
    Queued,
    DelayedFair,
    Inactive,
}

impl PolicyApplication {
    pub(super) const fn from_rq_state(state: OwnerRqTaskState, owner_now_ns: u64) -> Self {
        match state {
            OwnerRqTaskState::Current => Self::Current { owner_now_ns },
            OwnerRqTaskState::Queued { .. } => Self::Queued,
            OwnerRqTaskState::DelayedFair { .. } => Self::DelayedFair,
            OwnerRqTaskState::Inactive => Self::Inactive,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeTransition {
    Notified,
    Activate,
}

pub(super) struct OwnerDispatchCommit {
    overrun_work: Option<Arc<ThreadCore>>,
}

impl OwnerDispatchCommit {
    pub(super) const NONE: Self = Self { overrun_work: None };

    pub(super) const fn has_deferred_task_lock_work(&self) -> bool {
        self.overrun_work.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OwnerEnqueueCommit {
    reschedule: Option<RescheduleKind>,
    scheduler_deadline_refresh_required: bool,
    effective_policy: SchedulePolicy,
    push_class: Option<RootDomainPushClass>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::system::task_system) struct OwnerReadyEnqueue {
    pub(in crate::system::task_system) reschedule: Option<RescheduleKind>,
    pub(in crate::system::task_system) scheduler_deadline_refresh_required: bool,
}

mod bandwidth;
mod current;
mod policy;
mod wake;
