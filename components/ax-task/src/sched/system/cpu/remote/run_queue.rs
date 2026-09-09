use core::ops::Deref;

use super::*;
use crate::{
    sched::algorithm::{EnqueueReason, FairEntity, LinkedRqTaskRef, SchedulerClass},
    thread::WakeIntent,
};

/// Typed reason for entering the per-CPU runqueue with irqsave semantics.
///
/// Scheduler-frame and offline-bootstrap owners use the separate
/// `lock_run_queue_irq_disabled` contract and therefore never appear here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunQueueGuardSource {
    Transaction,
    OwnerCurrentThreadObservation,
    OwnerCurrentCoreObservation,
    OwnerRunnableObservation,
    TimerDeadlineDerivationObservation,
    RtAccounting,
    DeadlineAccounting,
    Membarrier,
    Lifecycle,
}

impl RunQueueGuardSource {
    pub(crate) const fn irq_guard_source(self) -> crate::runtime::IrqGuardSource {
        match self {
            Self::Transaction => crate::runtime::IrqGuardSource::CpuRunQueueTransactionTicket,
            Self::OwnerCurrentThreadObservation => {
                crate::runtime::IrqGuardSource::CpuRunQueueOwnerCurrentThreadObservationTicket
            }
            Self::OwnerCurrentCoreObservation => {
                crate::runtime::IrqGuardSource::CpuRunQueueOwnerCurrentCoreObservationTicket
            }
            Self::OwnerRunnableObservation => {
                crate::runtime::IrqGuardSource::CpuRunQueueOwnerRunnableObservationTicket
            }
            Self::TimerDeadlineDerivationObservation => {
                crate::runtime::IrqGuardSource::CpuRunQueueTimerDeadlineDerivationObservationTicket
            }
            Self::RtAccounting => crate::runtime::IrqGuardSource::CpuRunQueueRtAccountingTicket,
            Self::DeadlineAccounting => {
                crate::runtime::IrqGuardSource::CpuRunQueueDeadlineAccountingTicket
            }
            Self::Membarrier => crate::runtime::IrqGuardSource::CpuRunQueueMembarrierTicket,
            Self::Lifecycle => crate::runtime::IrqGuardSource::CpuRunQueueLifecycleTicket,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WakePreemptionDecision {
    KeepCurrent,
    DedicatedIdlePreempted,
    WakeeSelected,
    QueuedCandidateSelected,
}

/// Target-rq action selected by Linux's equal-priority RT wakeup rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EqualRtWakeAction {
    /// Preserve FIFO order because the current cannot move or the wakee can.
    PreserveFifoOrder,
    /// Put the wakee first so the next schedule can push the current away.
    RequeueWakeeAndReschedule,
}

/// Linux wake flags and target-rq facts that qualify wakeup preemption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WakePreemptionContext {
    intent: WakeIntent,
    equal_rt_action: EqualRtWakeAction,
    reschedule_pending: bool,
}

impl WakePreemptionContext {
    pub(crate) const fn new(
        intent: WakeIntent,
        equal_rt_action: EqualRtWakeAction,
        reschedule_pending: bool,
    ) -> Self {
        Self {
            intent,
            equal_rt_action,
            reschedule_pending,
        }
    }

    const fn normal() -> Self {
        Self::new(
            WakeIntent::Normal,
            EqualRtWakeAction::PreserveFifoOrder,
            false,
        )
    }
}

/// Owner-rq facts committed by one runnable-task insertion.
///
/// Preemption remains a complete-rq decision made after insertion. This
/// outcome separately preserves whether the insertion made the current
/// entity's runtime deadline newly relevant, so a `KeepCurrent` decision can
/// still ask the owner CPU to rederive its physical clockevent.
#[must_use = "owner enqueue facts must be consumed before publishing scheduler work"]
pub(crate) struct OwnerRqEnqueue {
    entity: SchedulingEntity,
    scheduler_deadline_refresh_required: bool,
}

impl OwnerRqEnqueue {
    pub(crate) const fn entity(&self) -> &SchedulingEntity {
        &self.entity
    }

    pub(crate) const fn scheduler_deadline_refresh_required(&self) -> bool {
        self.scheduler_deadline_refresh_required
    }
}

/// Runtime-accounting outcome for the task currently installed in `rq`.
///
/// Dedicated idle is a separate scheduler class in Linux and must not flow
/// through task utilization or RT bandwidth accounting. Encoding that split
/// in the result prevents callers from reconstructing idle identity after the
/// class hook has advanced its execution timestamp.
pub(in crate::sched::system::cpu) enum RqCurrentUpdate {
    DedicatedIdle,
    Task {
        charge: DispatchCharge,
        reschedule: Option<RescheduleKind>,
        realtime: bool,
        rt_quota_exempt: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CurrentAccountingEvent {
    RuntimeUpdate,
    ClockEvent,
    SchedulerTick { tick_ns: u64 },
    SchedulerTickWithClockEvent { tick_ns: u64 },
}

impl CurrentAccountingEvent {
    const fn runs_class_tick(self, slice_expired: bool) -> bool {
        matches!(
            self,
            Self::SchedulerTick { .. } | Self::SchedulerTickWithClockEvent { .. }
        ) || slice_expired
    }

    const fn periodic_tick_ns(self) -> Option<u64> {
        match self {
            Self::SchedulerTick { tick_ns } | Self::SchedulerTickWithClockEvent { tick_ns } => {
                Some(tick_ns)
            }
            Self::RuntimeUpdate | Self::ClockEvent => None,
        }
    }

    const fn class_reschedule_kind(
        self,
        policy: SchedulePolicy,
        slice_expired: bool,
    ) -> RescheduleKind {
        match (self, policy, slice_expired) {
            // Linux's Fair hrtick callback invokes task_tick(..., queued=1).
            // entity_tick() first performs the lazy update_curr() accounting,
            // then upgrades the queued hrtick expiry with resched_curr().
            (Self::ClockEvent | Self::SchedulerTickWithClockEvent { .. }, _, true) => {
                RescheduleKind::Immediate
            }
            (_, SchedulePolicy::Fair { .. }, _) => RescheduleKind::Lazy,
            _ => RescheduleKind::Immediate,
        }
    }
}

impl WakePreemptionDecision {
    pub(crate) const fn requests_reschedule(self) -> bool {
        matches!(self, Self::DedicatedIdlePreempted | Self::WakeeSelected)
    }

    /// Maps the class decision to Linux PREEMPT_RT's reschedule flag.
    pub(crate) const fn reschedule_kind(
        self,
        wakee_policy: SchedulePolicy,
    ) -> Option<RescheduleKind> {
        match self {
            Self::KeepCurrent | Self::QueuedCandidateSelected => None,
            // Linux always upgrades an idle-current wake to ordinary
            // `TIF_NEED_RESCHED`, even when the waking class is Fair.
            Self::DedicatedIdlePreempted => Some(RescheduleKind::Immediate),
            Self::WakeeSelected => Some(match wakee_policy {
                SchedulePolicy::Fair { .. } => RescheduleKind::Lazy,
                _ => RescheduleKind::Immediate,
            }),
        }
    }
}

/// Scheduler state protected by the target CPU's irqsave runqueue lock.
///
/// Mutable runtime accounting and switch-tail state remain owner-only in
/// [`CpuLocal`]. The current scheduling snapshot is committed here with
/// physical queue membership so a remote waker can evaluate preemption.
#[derive(Debug)]
pub(crate) struct CpuRunQueueState {
    owner: CpuId,
    clock: RunQueueClock,
    queue: RunQueue,
    rt_throttled: bool,
    idle: Option<IdleRqTask>,
    membarrier_state: AddressSpaceMembarrierState,
    published_domain: Option<RunQueueDomainPublication>,
    published_load: Option<RunQueueLoadPublication>,
}

/// Root-domain state derived from one rq-lock transaction.
///
/// Linux updates cpupri, cpudl, overload, and NOHZ masks from the scheduling
/// class which changed the corresponding rq fact. Keeping the last committed
/// values with the rq owner provides the same edge-triggered publication
/// without making every transaction reread every root-domain mirror.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunQueueDomainPublication {
    pub(crate) online: bool,
    pub(crate) highest_rt_priority: Option<u8>,
    pub(crate) earliest_deadline: Option<u64>,
    pub(crate) pushable_realtime: bool,
    pub(crate) pushable_deadline: bool,
    pub(crate) pushable_fair: bool,
}

/// Per-CPU idle task state owned by rq but never linked as a class entity.
///
/// This mirrors Linux's idle scheduling class: idle remains logically on its
/// rq while staying outside class queues, `nr_running`, and load accounting.
#[derive(Debug)]
struct IdleRqTask {
    core: Arc<ThreadCore>,
    active: Option<ActiveSchedulingState>,
    metadata: RqTaskMetadata,
    rt_quota_exempt: bool,
}

impl CpuRunQueueState {
    pub(crate) fn new(owner: CpuId, config: TaskSystemConfig) -> Self {
        Self {
            owner,
            clock: RunQueueClock::new(),
            queue: RunQueue::configured(
                u64::from(config.deadline_cap_percent()) * 10_000_000,
                config.thread_capacity(),
            ),
            rt_throttled: false,
            idle: None,
            membarrier_state: AddressSpaceMembarrierState::NONE,
            published_domain: None,
            published_load: None,
        }
    }

    pub(crate) fn take_domain_publication(
        &mut self,
        online: bool,
    ) -> Option<(Option<RunQueueDomainPublication>, RunQueueDomainPublication)> {
        let publication = RunQueueDomainPublication {
            online,
            highest_rt_priority: self.highest_rt_priority_including_current(),
            earliest_deadline: self.earliest_deadline_including_current(),
            pushable_realtime: online && self.has_pushable_realtime(),
            pushable_deadline: online && self.has_pushable_deadline(),
            pushable_fair: online && self.has_pushable_fair(),
        };
        if self.published_domain == Some(publication) {
            return None;
        }
        let previous = self.published_domain.replace(publication);
        Some((previous, publication))
    }

    pub(crate) fn invalidate_domain_publication(&mut self) {
        self.published_domain = None;
        self.published_load = None;
        self.queue.mark_publication_dirty();
    }

    pub(crate) fn take_summary_dirty(&mut self, online: bool) -> bool {
        let queue_dirty = self.queue.take_publication_dirty();
        queue_dirty
            || self.published_load.is_none()
            || self
                .published_domain
                .is_none_or(|publication| publication.online != online)
    }

    pub(crate) fn take_load_publication(
        &mut self,
        publication: RunQueueLoadPublication,
    ) -> Option<(Option<RunQueueLoadPublication>, RunQueueLoadPublication)> {
        if self.published_load == Some(publication) {
            return None;
        }
        let previous = self.published_load.replace(publication);
        Some((previous, publication))
    }

    /// Updates and snapshots Linux-style `rq->clock` under this runqueue lock.
    pub(crate) fn update_clock(&mut self) -> RunQueueClockSnapshot {
        let sample = task_runtime::rq_clock_sample();
        self.clock.update(sample)
    }

    /// Reserves class-node storage before the task is published.
    ///
    /// This changes only cold structural capacity, never runnable state, and
    /// therefore deliberately precedes the first owner-rq transaction for the
    /// new task. Linux obtains the same property by embedding class nodes in
    /// `task_struct` before publication.
    pub(crate) fn prepare_thread_slot(&mut self, slot: usize) {
        self.queue.prepare_thread_slot(slot);
    }

    /// Grants the owner-rq transaction access to scheduler-class mutations.
    ///
    /// Visibility is intentionally limited to the CPU scheduler module: task
    /// system code must express every runnable-state change through
    /// `OwnerRqTxn` rather than a raw runqueue guard.
    pub(in crate::sched::system::cpu) fn owner_transaction_queue_mut(&mut self) -> &mut RunQueue {
        &mut self.queue
    }
}

fn fair_preemption_cancels_protection(
    current_policy: SchedulePolicy,
    current_entity: &SchedulingEntity,
    wakee_policy: SchedulePolicy,
    wakee_entity: &SchedulingEntity,
) -> bool {
    let (
        SchedulePolicy::Fair {
            mode: current_mode, ..
        },
        SchedulePolicy::Fair {
            mode: wakee_mode, ..
        },
        Some(current),
        Some(wakee),
    ) = (
        current_policy,
        wakee_policy,
        current_entity.fair(),
        wakee_entity.fair(),
    )
    else {
        return false;
    };
    (current_mode == FairMode::Idle && wakee_mode != FairMode::Idle)
        || (current_mode == wakee_mode && wakee.has_shorter_slice_than(current))
}

impl Deref for CpuRunQueueState {
    type Target = RunQueue;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

mod membership;

mod current;

mod runtime_state;

mod accounting;

mod idle;

mod deadline;

mod preemption;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::Nice;

    const FAIR_POLICY: SchedulePolicy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    const FAIR_HRTICK_RESCHEDULE: RescheduleKind =
        CurrentAccountingEvent::ClockEvent.class_reschedule_kind(FAIR_POLICY, true);
    const FAIR_COALESCED_TICK_RESCHEDULE: RescheduleKind =
        CurrentAccountingEvent::SchedulerTickWithClockEvent { tick_ns: 10 }
            .class_reschedule_kind(FAIR_POLICY, true);

    const _: () = assert!(matches!(FAIR_HRTICK_RESCHEDULE, RescheduleKind::Immediate));
    const _: () = assert!(matches!(
        FAIR_COALESCED_TICK_RESCHEDULE,
        RescheduleKind::Immediate
    ));

    #[test]
    fn fair_class_runtime_expiry_uses_immediate_hrtick_semantics() {
        let event = CurrentAccountingEvent::ClockEvent;

        assert!(event.runs_class_tick(true));
        assert_eq!(
            FAIR_HRTICK_RESCHEDULE,
            RescheduleKind::Immediate,
            "Fair hrtick queued accounting upgrades the lazy update to ordinary rescheduling",
        );
        assert_eq!(
            FAIR_COALESCED_TICK_RESCHEDULE,
            RescheduleKind::Immediate,
            "a periodic tick coalesced with Fair hrtick must retain queued hrtick semantics",
        );
        assert_eq!(
            CurrentAccountingEvent::SchedulerTick { tick_ns: 10 }
                .class_reschedule_kind(FAIR_POLICY, false),
            RescheduleKind::Lazy,
            "a periodic Fair class check without hrtick expiry remains lazy",
        );
    }
}
