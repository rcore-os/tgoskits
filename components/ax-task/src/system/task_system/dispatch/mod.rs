//! Wake consumption, runqueue dispatch, and policy-application internals.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct PolicyGenerationCommit {
    pub(super) base_policy: SchedulePolicy,
    pub(super) running_policy_changed: bool,
    pub(super) held_deadline_reservation: u64,
    pub(super) committed_deadline_reservation: u64,
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
