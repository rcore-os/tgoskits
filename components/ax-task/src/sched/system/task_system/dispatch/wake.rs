//! Direct wakeup transactions.

use super::*;
use crate::{
    runtime::{cpu::SchedulerRuntimeDeadline, lock::IrqOwner},
    sched::{algorithm::FairEntity, system::WakePreemptionContext},
};

struct FairWakeContext<'a> {
    affinity: &'a CpuSet,
    waker: Option<CpuId>,
    previous: Option<CpuId>,
    wakee_demand: u64,
    intent: WakeIntent,
    wakee_is_idle: bool,
    wake_wide: bool,
}

#[derive(Clone, Copy)]
struct WakeTransactionContext {
    producer: CpuId,
}

impl WakeTransactionContext {
    fn current() -> Self {
        Self {
            producer: CpuId::new(unsafe { task_runtime::current_cpu_id().as_u32() }),
        }
    }
}

struct FairWakeAffineContext {
    waker: CpuId,
    previous: CpuId,
    sync: bool,
    waker_idle: bool,
    previous_idle: bool,
    waker_is_only_runnable: bool,
    waker_demand: u64,
    previous_demand: u64,
}

#[derive(Debug, Eq, PartialEq)]
enum WakeTargetSelection {
    Pinned(CpuId),
    SchedulerClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OnRqWakeAction {
    ReactivateDelayedFair,
    PublishAlreadyQueued,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OnRqRevalidation {
    CommitOnRq,
    ActivateOffRq,
}

enum WakeActivationPreparation {
    Throttled,
    Ready {
        policy: SchedulePolicy,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        current_fair: Option<FairEntity>,
        maintains_fair_virtual_time: bool,
        delayed_migration_wake: bool,
        deadline_wake: bool,
    },
}

/// Selects the Linux `ttwu_runnable()` action after the rq lock is held.
fn on_rq_wake_action(delayed_fair: bool) -> OnRqWakeAction {
    if delayed_fair {
        OnRqWakeAction::ReactivateDelayedFair
    } else {
        OnRqWakeAction::PublishAlreadyQueued
    }
}

/// Mirrors Linux's `!task_on_cpu(rq, p)` wakeup-preemption gate.
fn on_rq_wake_preemption_required(on_cpu: bool) -> bool {
    !on_cpu
}

/// Revalidates Linux's `task_on_rq_queued()` fact under the rq lock.
fn on_rq_revalidation(scheduler_owned: bool) -> OnRqRevalidation {
    if scheduler_owned {
        OnRqRevalidation::CommitOnRq
    } else {
        OnRqRevalidation::ActivateOffRq
    }
}

/// Returns the scheduler policy needed by the on-rq wake path.
///
/// A concurrent dequeue invalidates the scheduler-owned state before the
/// waker acquires the run-queue transaction. That `ActivateOffRq` path must
/// therefore not require a policy; only the still-queued path needs it for
/// class-specific accounting and preemption.
fn wake_policy_for_revalidation(
    scheduling_policy: Option<&SchedulePolicy>,
    revalidation: OnRqRevalidation,
) -> Option<(SchedulePolicy, bool)> {
    match revalidation {
        OnRqRevalidation::ActivateOffRq => None,
        OnRqRevalidation::CommitOnRq => scheduling_policy.map(|policy| {
            let policy = *policy;
            (policy, matches!(policy, SchedulePolicy::Fair { .. }))
        }),
    }
}

/// Mirrors the generic Linux `select_task_rq()` gate before scheduler-class
/// placement runs.
fn wake_target_selection(affinity: &CpuSet) -> WakeTargetSelection {
    affinity.sole_cpu().map_or(
        WakeTargetSelection::SchedulerClass,
        WakeTargetSelection::Pinned,
    )
}

fn select_fair_wake_affine_cpu(context: FairWakeAffineContext) -> CpuId {
    let FairWakeAffineContext {
        waker,
        previous,
        sync,
        waker_idle,
        previous_idle,
        waker_is_only_runnable,
        waker_demand,
        previous_demand,
    } = context;
    if waker_idle {
        return if previous_idle { previous } else { waker };
    }
    if sync && waker_is_only_runnable {
        return waker;
    }
    if previous_idle {
        return previous;
    }
    if waker_demand < previous_demand || (sync && waker_demand == previous_demand) {
        waker
    } else {
        previous
    }
}

struct EqualRtWakeContext<'a> {
    target: CpuId,
    current: &'a CurrentDispatch,
    wakee_policy: SchedulePolicy,
    wakee_affinity: &'a CpuSet,
    reschedule_pending: bool,
}

impl TaskSystem {
    fn publish_detached_deadline_owner_work(source_remote: &CpuRemote) -> bool {
        source_remote.kick_scheduler_work()
    }

    fn consume_wake_locked(core: &Arc<ThreadCore>) -> WakeTransition {
        let (lifecycle, pending) =
            core.consume_wake_and_transition(true, Some(ThreadState::Waking));
        if !pending || lifecycle == ThreadState::Exited {
            return WakeTransition::Notified;
        }
        match lifecycle {
            ThreadState::Parking => WakeTransition::Notified,
            ThreadState::Blocked => WakeTransition::Activate,
            ThreadState::Running | ThreadState::Waking => WakeTransition::Notified,
            ThreadState::New | ThreadState::Exited => WakeTransition::Notified,
        }
    }

    /// Consumes a wake for Linux's `ttwu_runnable()` on-rq path.
    ///
    /// The task remains Blocked while its rq membership is revalidated and a
    /// possible delayed Fair dequeue is cancelled under the rq lock. That
    /// transaction publishes Running directly; Waking is reserved for the
    /// off-rq path that must drop the task lock before target selection and
    /// enqueue.
    fn consume_on_rq_wake_locked(core: &Arc<ThreadCore>) -> WakeTransition {
        let (lifecycle, pending) = core.consume_wake_and_transition(false, None);
        if !pending || lifecycle != ThreadState::Blocked {
            WakeTransition::Notified
        } else {
            WakeTransition::Activate
        }
    }
}

mod placement;

mod request;

mod on_run_queue;

mod activation;

mod enqueue;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_sync_wake_affine_prefers_singleton_waker_rq() {
        let waker = CpuId::new(0);
        let previous = CpuId::new(1);

        assert_eq!(
            select_fair_wake_affine_cpu(FairWakeAffineContext {
                waker,
                previous,
                sync: true,
                waker_idle: false,
                previous_idle: false,
                waker_is_only_runnable: true,
                waker_demand: 2_048,
                previous_demand: 1_024,
            }),
            waker,
        );
    }

    #[test]
    fn linux_singleton_affinity_skips_scheduler_class_wake_selection() {
        let pinned = CpuId::new(2);
        let mut affinity = CpuSet::empty(4);
        assert!(affinity.insert(pinned));

        assert_eq!(
            wake_target_selection(&affinity),
            WakeTargetSelection::Pinned(pinned),
        );
    }

    #[test]
    fn linux_on_rq_wake_keeps_an_already_queued_task_linked() {
        assert_eq!(
            on_rq_wake_action(false),
            OnRqWakeAction::PublishAlreadyQueued,
        );
    }

    #[test]
    fn linux_on_rq_wake_skips_preemption_for_the_current_task() {
        assert!(!on_rq_wake_preemption_required(true));
    }

    #[test]
    fn linux_on_rq_wake_falls_back_after_a_concurrent_dequeue() {
        assert_eq!(on_rq_revalidation(false), OnRqRevalidation::ActivateOffRq,);
    }

    #[test]
    fn linux_on_rq_wake_does_not_require_policy_after_a_concurrent_dequeue() {
        assert_eq!(
            wake_policy_for_revalidation(None, OnRqRevalidation::ActivateOffRq),
            None,
        );
    }
}
