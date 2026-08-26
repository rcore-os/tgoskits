//! Per-CPU runqueue owned by the target CPU's IRQ-safe scheduler lock.

use alloc::{sync::Arc, vec::Vec};

mod accounting;
mod balance;
mod class;
mod deadline;
mod deadline_pushable;
mod dispatch;
mod lifecycle;
mod membership;
mod realtime;
mod task;

pub(crate) use balance::BalanceScan;
pub(crate) use class::{SchedulerClass, default_sync_wakeup_preempts, wakeup_preempts};
use deadline::{DeadlineQueueKey, DeadlineRunQueue};
use realtime::{RealtimeQueueKey, RealtimeRunQueue};
pub(crate) use task::{
    LinkedPickedThread, PickTaskResult, PickedThread, QueuedThread, QueuedThreadSnapshot,
    RqTaskMetadata, RunQueueNodeStorage,
};

use super::fair_queue::{FairPick, FairRunQueue};
use crate::{
    CurrentDispatch, DispatchCharge, FairEntity, SchedulePolicy, SchedulingClass, SchedulingEntity,
    TaskError, ThreadCore, ThreadId,
};

/// Why a runnable thread is being inserted into its owner run queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueReason {
    /// Newly ready or awakened work joins the class tail.
    Wake,
    /// An explicit yield joins the class tail.
    Yield,
    /// Higher-class preemption preserves FIFO/RR position.
    Preempted,
    /// A replenished reservation becomes eligible again.
    Replenished,
    /// Runnable state was handed off by another owner CPU without a new wake.
    Migrated,
    /// The owner CPU applied a newer scheduling-policy generation.
    PolicyChanged,
}

impl EnqueueReason {
    /// Returns whether this enqueue is allowed to challenge `rq->curr`.
    ///
    /// Linux runs `check_preempt_curr()` for a wakeup, replenishment,
    /// migration, or queued priority change. `put_prev_task()` instead returns
    /// the outgoing current to its class without treating it as a newly woken
    /// competitor; doing so would manufacture another reschedule request from
    /// the scheduling decision that is already in progress.
    pub(crate) const fn checks_preemption_after_enqueue(self) -> bool {
        matches!(
            self,
            Self::Wake | Self::Replenished | Self::Migrated | Self::PolicyChanged
        )
    }
}

/// Which fixed-priority RT entities are eligible in this owner selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RtEligibility {
    /// Linux `rt_rq_throttled()` is false for this runqueue.
    Runnable,
    /// No boosted entity keeps this runqueue runnable after quota exhaustion.
    Throttled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueMembershipClass {
    Stop,
    Deadline(DeadlineQueueKey),
    /// Linux `p->on_rq == TASK_ON_RQ_QUEUED` while `dl_throttled` keeps the
    /// entity outside `dl_rq->root` and `rq->nr_running`.
    DeadlineThrottled,
    Realtime(RealtimeQueueKey),
    /// Linux keeps SCHED_IDLE in the same `cfs_rq` as Normal and Batch: every
    /// fair-policy mode shares this membership and the single EEVDF tree.
    Fair,
}

impl QueueMembershipClass {
    const fn scheduler_class(self) -> SchedulerClass {
        match self {
            Self::Stop => SchedulerClass::Stop,
            Self::Deadline(_) | Self::DeadlineThrottled => SchedulerClass::Deadline,
            Self::Realtime(_) => SchedulerClass::Realtime,
            Self::Fair => SchedulerClass::Fair,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueMembership {
    generation: u32,
    class: QueueMembershipClass,
}

const fn fixed_placement_demand(policy: SchedulePolicy) -> u64 {
    policy
        .placement_demand()
        .saturating_sub(policy.fair_demand())
}

const fn retains_running_link(policy: SchedulePolicy) -> bool {
    matches!(
        policy,
        SchedulePolicy::Deadline(_)
            | SchedulePolicy::Fifo { .. }
            | SchedulePolicy::RoundRobin { .. }
    )
}

#[derive(Debug)]
pub(crate) struct RunQueue {
    /// Linux `rq->curr`: the sole running-task identity owned by this rq.
    ///
    /// RT/DL retain their class nodes while running, but those nodes do not
    /// carry a second "current" marker. Every class operation derives current
    /// status from this dispatch token while holding the rq lock.
    current: Option<CurrentDispatch>,
    stop: Option<QueuedThread>,
    deadline: DeadlineRunQueue,
    rt: RealtimeRunQueue,
    /// Linux `rq->cfs`: one EEVDF tree for Normal, Batch, and SCHED_IDLE work.
    /// SCHED_IDLE competes with `WEIGHT_IDLEPRIO` inside this tree instead of
    /// owning a separate class.
    fair: FairRunQueue,
    membership: Vec<Option<QueueMembership>>,
    fixed_placement_demand: u64,
    balance_scan_epoch: u64,
    next_sequence: u64,
    /// Linux `rq->nr_running`: runnable non-idle tasks, including current.
    nr_running: usize,
}

impl RunQueue {
    pub(crate) fn configured(deadline_max_bw_scaled: u64, thread_capacity: usize) -> Self {
        Self {
            current: None,
            stop: None,
            deadline: DeadlineRunQueue::new(deadline_max_bw_scaled, thread_capacity),
            rt: RealtimeRunQueue::new(),
            fair: FairRunQueue::new(),
            membership: Vec::new(),
            fixed_placement_demand: 0,
            balance_scan_epoch: 0,
            next_sequence: 0,
            nr_running: 0,
        }
    }

    pub(crate) const fn current(&self) -> Option<&CurrentDispatch> {
        self.current.as_ref()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.current.as_mut()
    }

    pub(crate) fn install_current(&mut self, current: CurrentDispatch) {
        assert!(
            self.current.replace(current).is_none(),
            "rq->curr must be cleared before installing a successor"
        );
    }

    pub(crate) fn take_current(&mut self) -> Option<CurrentDispatch> {
        let mut current = self.current.take()?;
        current.finish_runtime_interval();
        Some(current)
    }

    fn linked_current(&self) -> Option<ThreadId> {
        let current = self.current.as_ref()?.thread();
        matches!(
            self.membership_class(current),
            Some(QueueMembershipClass::Deadline(_) | QueueMembershipClass::Realtime(_))
        )
        .then_some(current)
    }
}
