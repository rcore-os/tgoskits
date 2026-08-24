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

#[cfg(any(test, all(axtest, feature = "axtest")))]
std::thread_local! {
    static WAKE_TARGET_SELECTIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static OWNER_DISPATCH_CONSTRUCTIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn reset_wake_target_selections() {
    WAKE_TARGET_SELECTIONS.set(0);
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn wake_target_selections() -> usize {
    WAKE_TARGET_SELECTIONS.get()
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn reset_owner_dispatch_constructions() {
    OWNER_DISPATCH_CONSTRUCTIONS.set(0);
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn owner_dispatch_constructions() -> usize {
    OWNER_DISPATCH_CONSTRUCTIONS.get()
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
    const NONE: Self = Self { overrun_work: None };

    pub(super) const fn has_deferred_task_lock_work(&self) -> bool {
        self.overrun_work.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OwnerEnqueueCommit {
    preempts_current: bool,
    scheduler_deadline_refresh_required: bool,
    effective_policy: SchedulePolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::system::task_system) struct OwnerReadyEnqueue {
    pub(in crate::system::task_system) preempts_current: bool,
    pub(in crate::system::task_system) scheduler_deadline_refresh_required: bool,
}

mod bandwidth;
mod current;
mod policy;
mod wake;

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) use wake::{
    arm_wake_before_thread_lock_race, arm_wake_during_final_park_publication,
    complete_wake_before_thread_lock_race, complete_wake_during_final_park_publication,
    wake_before_thread_lock_race_entered, wake_during_final_park_publication_entered,
};
