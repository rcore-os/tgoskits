use core::ops::Deref;

use super::*;
use crate::{EnqueueReason, FairEntity, SchedulerClass, WakeIntent};

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
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    TestInspection,
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
            #[cfg(any(test, all(axtest, feature = "axtest")))]
            Self::TestInspection => crate::runtime::IrqGuardSource::CpuRunQueueLifecycleTicket,
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
}

impl WakePreemptionContext {
    pub(crate) const fn new(intent: WakeIntent, equal_rt_action: EqualRtWakeAction) -> Self {
        Self {
            intent,
            equal_rt_action,
        }
    }

    const fn normal() -> Self {
        Self::new(WakeIntent::Normal, EqualRtWakeAction::PreserveFifoOrder)
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
pub(in crate::system::cpu) enum RqCurrentUpdate {
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
    SchedulerTick,
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
        }
    }

    /// Updates and snapshots Linux-style `rq->clock` under this runqueue lock.
    pub(crate) fn update_clock(&mut self) -> RunQueueClockSnapshot {
        let sample = task_runtime::rq_clock_sample(RuntimeCpuId::new(self.owner.as_u32()));
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
    pub(in crate::system::cpu) fn owner_transaction_queue_mut(&mut self) -> &mut RunQueue {
        &mut self.queue
    }

    pub(in crate::system::cpu) fn enqueue_task(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self.queue.enqueue_task(thread, reason, current_fair)?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    pub(in crate::system::cpu) fn take_delayed_fair_for_update(
        &mut self,
        thread: ThreadId,
    ) -> Option<QueuedThread> {
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let delayed = self.queue.take_delayed_fair_for_update(thread)?;
        self.queue.update_fair_virtual_time(current_fair);
        Some(delayed)
    }

    pub(in crate::system::cpu) fn restore_delayed_fair_after_update(
        &mut self,
        thread: QueuedThread,
    ) -> SchedulingEntity {
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let entity = self.queue.restore_delayed_fair_after_update(thread);
        self.tighten_current_fair_slice_protection(&entity);
        self.queue.update_fair_virtual_time(current_fair);
        entity
    }

    pub(in crate::system::cpu) fn finish_detached_delayed_fair(
        &mut self,
        active: &mut ActiveSchedulingState,
        timing_granularity_ns: u64,
    ) {
        let current_fair = self.current_fair_contender();
        self.queue
            .finish_detached_delayed_fair(active, timing_granularity_ns);
        self.queue.update_fair_virtual_time(current_fair);
    }

    pub(in crate::system::cpu) fn enqueue_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self
            .queue
            .enqueue_delayed_fair_transfer(thread, current_fair)?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_before
                != runtime_timer_required_after
                || runtime_timer_delta_before != runtime_timer_delta_after,
        })
    }

    pub(in crate::system::cpu) fn enqueue_reactivated_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self.queue.enqueue_reactivated_delayed_fair_transfer(
            thread,
            current_fair,
            timing_granularity_ns,
        )?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    fn tighten_current_fair_slice_protection(&mut self, wakee_entity: &SchedulingEntity) {
        let Some(wakee) = wakee_entity
            .fair()
            .filter(|fair| fair.mode() == FairMode::Normal)
        else {
            return;
        };
        let Some(shortest_queued_slice_ns) = self.queue.min_fair_service_request_ns(wakee.mode())
        else {
            return;
        };
        let Some(current) = self
            .current_scheduling_entity_mut()
            .and_then(|entity| match entity {
                SchedulingEntity::Fair(fair) if fair.mode() == wakee.mode() => Some(fair),
                _ => None,
            })
        else {
            return;
        };
        current.update_slice_protection(shortest_queued_slice_ns);
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn set_virtual_time_for_test(&mut self, virtual_time: u64) {
        self.queue.set_virtual_time_for_test(virtual_time);
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        self.queue.update_fair_virtual_time(current);
    }

    pub(crate) const fn current(&self) -> Option<&CurrentDispatch> {
        self.queue.current()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.queue.current_mut()
    }

    pub(crate) fn current_scheduling_entity(&self) -> Option<&SchedulingEntity> {
        let current = self.queue.current()?;
        self.queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity_ref())
    }

    /// Returns the running Fair entity that participates in EEVDF accounting.
    ///
    /// Linux keeps its dedicated idle task in `idle_sched_class`, completely
    /// outside `cfs_rq::{curr, sum_weight, sum_w_vruntime}`. Our idle dispatch
    /// still owns a Fair-shaped policy entity for uniform task metadata, so
    /// every Fair accounting boundary must exclude it explicitly.
    pub(crate) fn current_fair_contender(&self) -> Option<FairEntity> {
        if self
            .current()
            .is_some_and(CurrentDispatch::is_dedicated_idle)
        {
            return None;
        }
        self.current_scheduling_entity()
            .and_then(SchedulingEntity::fair)
    }

    pub(crate) fn current_scheduling_entity_mut(&mut self) -> Option<&mut SchedulingEntity> {
        let thread = self.current_thread()?;
        if self.queue.is_linked_current(thread) {
            return self.queue.linked_current_entity_mut(thread);
        }
        Some(self.queue.current_mut()?.active_mut().entity_mut())
    }

    /// Requeues a Fair/stop current using only its rq-owned dispatch state.
    pub(crate) fn put_prev_unlinked_current(
        &mut self,
        thread: ThreadId,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.current_thread() != Some(thread) || self.queue.is_linked_current(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let dispatch = self.queue.take_current().ok_or(TaskError::NotReady)?;
        let queued = dispatch
            .into_queued_thread()
            .ok_or(TaskError::InvalidConfiguration)?;
        let queued_entity = self.queue.enqueue_task(queued, reason, current_fair)?;
        // The requeued current is now part of the Fair tree. Including its old
        // snapshot again would count one task twice and move V away from the
        // weighted average used by Linux EEVDF eligibility.
        self.queue.update_fair_virtual_time(None);
        Ok(queued_entity)
    }

    /// Retains an ineligible Fair sleeper on-rq like Linux DELAY_DEQUEUE.
    pub(crate) fn delay_dequeue_unlinked_current(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
        force: bool,
    ) -> Option<SchedulingEntity> {
        if self.current_thread() != Some(thread) || self.queue.is_linked_current(thread) {
            return None;
        }
        let fair = self.current_fair_contender()?;
        self.queue.update_fair_virtual_time(Some(fair));
        let virtual_time = self.queue.virtual_time_for_mode(fair.mode());
        #[cfg(feature = "task-test-hooks")]
        let (fair, virtual_time) = if force && fair.is_eligible(virtual_time) {
            let SchedulingEntity::Fair(current) = self.current_scheduling_entity_mut()? else {
                return None;
            };
            current.force_ineligible_for_delayed_dequeue(virtual_time);
            let fair = *current;
            self.queue.update_fair_virtual_time(Some(fair));
            (fair, self.queue.virtual_time_for_mode(fair.mode()))
        } else {
            (fair, virtual_time)
        };
        if !force && fair.is_eligible(virtual_time) {
            return None;
        }
        let SchedulingEntity::Fair(current) = self.current_scheduling_entity_mut()? else {
            return None;
        };
        current.begin_delayed_dequeue(virtual_time, timing_granularity_ns);
        let dispatch = self.queue.take_current()?;
        let queued = dispatch.into_queued_thread()?;
        let entity = self.queue.enqueue_delayed_fair_current(queued);
        self.queue.update_fair_virtual_time(None);
        Some(entity)
    }

    pub(crate) fn is_delayed_fair(&self, thread: ThreadId) -> bool {
        self.queue.is_delayed_fair(thread)
    }

    pub(crate) fn finish_delayed_fair_dequeue(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        self.queue
            .finish_delayed_fair_dequeue(thread, timing_granularity_ns)
    }

    pub(crate) fn reactivate_delayed_fair(
        &mut self,
        thread: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<OwnerRqEnqueue> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity =
            self.queue
                .reactivate_delayed_fair(thread, current_fair, timing_granularity_ns)?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Some(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    pub(crate) fn install_current(&mut self, current: CurrentDispatch) {
        if self.queue.current().is_some() {
            task_runtime::fatal_invariant(0x5251_0001, self.owner.as_u32() as usize);
        }
        self.membarrier_state = Self::state_for_address_space(current.address_space());
        self.queue.install_current(current);
    }

    fn state_for_address_space(
        address_space: crate::runtime::AddressSpaceHandle,
    ) -> AddressSpaceMembarrierState {
        if address_space.is_none() {
            AddressSpaceMembarrierState::NONE
        } else {
            task_runtime::address_space_membarrier_state(address_space)
        }
    }

    pub(crate) const fn membarrier_state(&self) -> AddressSpaceMembarrierState {
        self.membarrier_state
    }

    /// Refreshes Linux `rq->membarrier_state` from the currently published
    /// dispatch. The full barrier pairs registration with user execution on
    /// this CPU and is intentionally inside the rq transaction.
    pub(crate) fn refresh_membarrier_state(&mut self) -> AddressSpaceMembarrierState {
        let state = self
            .current()
            .map_or(AddressSpaceMembarrierState::NONE, |current| {
                Self::state_for_address_space(current.address_space())
            });
        self.membarrier_state = state;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state
    }

    pub(crate) fn take_current(&mut self) -> Option<CurrentDispatch> {
        self.queue.take_current()
    }

    pub(crate) fn linked_current_entity_mut(
        &mut self,
        thread: ThreadId,
    ) -> Option<&mut SchedulingEntity> {
        self.queue.linked_current_entity_mut(thread)
    }

    pub(crate) fn scheduling_entity(&self, thread: ThreadId) -> Option<SchedulingEntity> {
        self.queue.scheduling_entity(thread).or_else(|| {
            self.queue
                .current()
                .filter(|current| current.thread() == thread)
                .and_then(CurrentDispatch::owned_scheduling_entity_ref)
                .cloned()
        })
    }

    pub(crate) fn base_scheduling_entity(&self, thread: ThreadId) -> Option<SchedulingEntity> {
        self.queue.base_scheduling_entity(thread).or_else(|| {
            self.queue
                .current()
                .filter(|current| current.thread() == thread)
                .and_then(CurrentDispatch::owned_base_scheduling_entity_ref)
                .cloned()
        })
    }

    /// Linux `update_entity_lag()` for a running task which is about to leave
    /// this rq. Fair current is owned by `rq->curr`; an RT/DL current retains
    /// its active state in the class structure. Both representations sample
    /// the source weighted virtual time before either owner is detached.
    pub(crate) fn capture_current_fair_migration(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) {
        let Some(fair) = self
            .base_scheduling_entity(thread)
            .and_then(|entity| entity.fair())
        else {
            return;
        };
        let virtual_time = self.queue.virtual_time_for_mode(fair.mode());
        if self
            .queue
            .capture_linked_fair_migration(thread, virtual_time, timing_granularity_ns)
        {
            return;
        }
        let current = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_100c, thread.as_u64() as usize)
            });
        current
            .active_mut()
            .base_entity_mut()
            .capture_fair_migration(virtual_time, timing_granularity_ns);
    }

    pub(crate) fn update_base_deadline_entity(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        self.queue.update_base_deadline_entity(thread, entity)
    }

    /// Returns the scheduler-class state owned by this rq.
    ///
    /// Fair and stopper current tasks keep the entity in `CurrentDispatch`;
    /// RT and Deadline current tasks remain linked in their class structures.
    /// This is the single owner-side query for both representations.
    pub(crate) fn scheduling_state(
        &self,
        thread: ThreadId,
    ) -> Option<(SchedulePolicy, SchedulingEntity)> {
        if let Some(current) = self
            .queue
            .current()
            .filter(|current| current.thread() == thread)
        {
            return self
                .current_scheduling_entity()
                .map(|entity| (current.schedule_policy(), entity.clone()));
        }
        self.queue.scheduling_state(thread)
    }

    pub(crate) fn current_runtime_timer_delta_ns(&self) -> Option<u64> {
        let current = self.queue.current()?;
        let entity = self
            .queue
            .linked_current_entity(current.thread())
            .or_else(|| current.owned_scheduling_entity_ref())
            .expect("current dispatch must have one rq-owned scheduling entity");
        CurrentDispatch::runtime_timer_delta_for(entity)
    }

    /// Returns whether the current entity contributes a runtime clockevent.
    ///
    /// Entities without a class runtime deadline do not need this clockevent.
    /// Like Linux EEVDF, a Fair current needs its slice timer only while a
    /// same-mode contender is queued. A first contender can therefore make
    /// this fact transition from false to true without requesting immediate
    /// wakeup preemption.
    pub(crate) fn current_runtime_timer_required(&self) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        if current.is_dedicated_idle() {
            return false;
        }
        let current_entity = self
            .current_scheduling_entity()
            .expect("current dispatch must have one rq-owned scheduling entity");
        if CurrentDispatch::runtime_timer_delta_for(current_entity).is_none() {
            return false;
        }
        current_entity.fair().is_none_or(|fair| {
            if fair.mode() == FairMode::Idle {
                self.has_idle_fair()
            } else {
                self.has_fair()
            }
        })
    }

    pub(crate) fn current_thread(&self) -> Option<ThreadId> {
        self.queue.current().map(CurrentDispatch::thread)
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.queue
            .current()
            .map(|dispatch| Arc::clone(dispatch.runtime_core_arc()))
    }

    pub(crate) fn update_current_runtime_binding(
        &mut self,
        thread: ThreadId,
        binding: crate::runtime::ThreadRuntimeBinding,
    ) -> Result<(), TaskError> {
        let next_membarrier_state = Self::state_for_address_space(binding.address_space());
        let current = self
            .queue
            .current_mut()
            .ok_or(TaskError::NoRunnableThread)?;
        if current.thread() != thread {
            return Err(TaskError::InvalidConfiguration);
        }
        current.update_runtime_binding(binding);
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            // Linux pairs exit_mm()/exec's rq->curr mm transition with
            // membarrier's entry/exit barriers before user execution resumes.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
        Ok(())
    }

    pub(crate) fn refresh_current_scheduler_metadata(
        &mut self,
        thread: ThreadId,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        let next_membarrier_state =
            Self::state_for_address_space(metadata.runtime_binding.address_space());
        let current = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_100d, thread.as_u64() as usize)
            });
        current.refresh_scheduler_metadata(metadata, rt_quota_exempt);
        if self.membarrier_state.identity() != next_membarrier_state.identity() {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        self.membarrier_state = next_membarrier_state;
    }

    pub(crate) fn update_thread_affinity(
        &mut self,
        thread: ThreadId,
        affinity: Arc<CpuSet>,
    ) -> bool {
        let current_updated = self
            .queue
            .current_mut()
            .filter(|current| current.thread() == thread)
            .map(|current| current.update_affinity(Arc::clone(&affinity)))
            .is_some();
        self.queue.update_affinity(thread, affinity) || current_updated
    }

    pub(crate) fn detach_current_schedule(
        &mut self,
        thread: ThreadId,
    ) -> Result<ActiveSchedulingState, TaskError> {
        if self.current_thread() != Some(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        if self.queue.is_linked_current(thread) {
            return self
                .queue
                .reclassify_task(thread)
                .map(QueuedThread::into_active)
                .ok_or(TaskError::NotReady);
        }
        self.queue
            .current_mut()
            .and_then(CurrentDispatch::take_owned_for_reclassify)
            .ok_or(TaskError::InvalidConfiguration)
    }

    pub(crate) fn install_current_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
        core: Arc<ThreadCore>,
        rt_quota_exempt: bool,
        migration_capable: bool,
        metadata: RqTaskMetadata,
    ) -> Result<(), TaskError> {
        if self.current_thread() != Some(thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        let linked = matches!(
            active.policy(),
            SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        );
        let policy = active.policy();
        if linked {
            self.queue.link_running(QueuedThread::new(
                thread,
                active,
                core,
                rt_quota_exempt,
                migration_capable,
                metadata,
            ))?;
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .install_reclassified_schedule(CurrentClassState::Linked { policy });
        } else {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .install_reclassified_schedule(CurrentClassState::Owned(active));
        }
        Ok(())
    }

    /// Accounts the running task in place under the rq lock.
    pub(in crate::system::cpu) fn update_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::RuntimeUpdate,
        )
    }

    /// Applies one Linux scheduler tick after accounting the running task.
    ///
    /// RT and Deadline entities remain linked in their class structure, while
    /// Fair/stop entities remain in `CurrentDispatch`. A clock tick therefore
    /// never has to take the dispatch out of the rq and reinstall it merely to
    /// update runtime, matching Linux `update_curr_*()` ownership.
    pub(in crate::system::cpu) fn task_tick_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::SchedulerTick,
        )
    }

    fn update_current_for_event(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
        event: CurrentAccountingEvent,
    ) -> Result<RqCurrentUpdate, TaskError> {
        let now_ns = self
            .clock
            .snapshot()
            .ok_or(TaskError::InvalidConfiguration)?
            .task()
            .as_nanos();
        let current_thread = self.current_thread().ok_or(TaskError::NoRunnableThread)?;
        if self.idle() == Some(current_thread) {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .account_dedicated_idle_until(now_ns);
            return Ok(RqCurrentUpdate::DedicatedIdle);
        }

        let bandwidth = self.queue.deadline_bandwidth();
        let (charge, policy, current_entity, rt_quota_exempt) = self.queue.charge_current(
            runtime_ns,
            now_ns,
            bandwidth.inactive_bw_scaled(),
            deadline_extra_bw_scaled,
            bandwidth.max_bw_scaled(),
            reclaimed_ns,
        )?;
        let deadline_replenish_reschedule = if charge.deadline_replenished {
            self.queue
                .requeue_replenished_deadline_current(current_thread)?
        } else {
            false
        };
        if let Some(current_fair) = current_entity.fair() {
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_current_fair_vtime_update(current_thread);
            self.queue.update_fair_virtual_time(Some(current_fair));
        }
        let class_tick_reschedule = match event {
            CurrentAccountingEvent::RuntimeUpdate => false,
            CurrentAccountingEvent::SchedulerTick => {
                SchedulerClass::for_policy(policy)
                    .task_tick(
                        &mut self.queue,
                        current_thread,
                        policy,
                        &current_entity,
                        charge,
                    )
                    .request_reschedule
            }
        };
        let deadline_runtime_reschedule =
            matches!(policy, SchedulePolicy::Deadline(_)) && charge.slice_expired;
        let request_reschedule =
            deadline_runtime_reschedule || deadline_replenish_reschedule || class_tick_reschedule;
        Ok(RqCurrentUpdate::Task {
            charge,
            reschedule: request_reschedule.then_some(match policy {
                SchedulePolicy::Fair { .. } => RescheduleKind::Lazy,
                _ => RescheduleKind::Immediate,
            }),
            realtime: matches!(
                policy,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            ),
            rt_quota_exempt,
        })
    }

    #[cfg(all(axtest, feature = "axtest"))]
    pub(crate) fn fifo_handoff_current_for_scheduler_deadline_test(
        config: TaskSystemConfig,
    ) -> Self {
        use crate::{
            ActiveSchedulingState, CurrentClassState, CurrentDispatchState, PickTaskResult,
            RqTaskTime, RtEligibility, RtPriority, SchedulingEntity,
        };

        let priority = RtPriority::new(10).expect("test priority must be valid");
        let policy = SchedulePolicy::fifo(priority);
        let current = ThreadId::from_parts(0, 1);
        let peer = ThreadId::from_parts(1, 1);
        let mut run_queue = Self::new(CpuId::new(0), config);

        for thread in [current, peer] {
            run_queue.prepare_thread_slot(thread.slot() as usize);
            let sched = Arc::new(crate::ThreadSchedCell::new_test(thread, policy));
            let core = Arc::new(ThreadCore::new(
                thread, policy, sched, None, None, None, None,
            ));
            let queued = QueuedThread::new(
                thread,
                ActiveSchedulingState::new(policy, SchedulingEntity::new(policy, 1, 0)),
                core,
                false,
                false,
                RqTaskMetadata::test(1),
            );
            let _enqueue = run_queue
                .enqueue_task(queued, EnqueueReason::Wake, None)
                .expect("test FIFO task must enqueue");
        }

        let PickTaskResult::Continue(picked) = run_queue
            .queue
            .pick_next_task(RtEligibility::Runnable, false)
            .expect("test FIFO queue must have a runnable task")
        else {
            panic!("FIFO test setup must not select a delayed Fair task");
        };
        assert_eq!(picked.id(), current);
        let metadata = picked.metadata().clone();
        let core = Arc::clone(picked.core());
        run_queue.queue.set_next_task(&picked);
        run_queue.install_current(CurrentDispatch::new(
            CurrentDispatchState {
                thread: current,
                schedule: CurrentClassState::Linked { policy },
                metadata,
                rt_quota_exempt: false,
            },
            &core,
            RqTaskTime::test(0),
        ));
        run_queue
    }

    #[cfg(all(axtest, feature = "axtest"))]
    pub(crate) fn exercise_rr_runtime_update_for_test()
    -> (ThreadId, ThreadId, ThreadId, bool, ThreadId, bool) {
        use crate::{
            ActiveSchedulingState, CurrentClassState, CurrentDispatchState, PickTaskResult,
            RqTaskTime, RtEligibility, RtPriority, SchedulerTimestamp, SchedulingEntity,
            runtime::RqClockSample,
        };

        let priority = RtPriority::new(10).expect("test priority must be valid");
        let policy = SchedulePolicy::round_robin_with_quantum(priority, 100)
            .expect("test quantum must be valid");
        let current = ThreadId::from_parts(0, 1);
        let peer = ThreadId::from_parts(1, 1);
        let mut run_queue = Self::new(CpuId::new(0), TaskSystemConfig::new(1));

        for thread in [current, peer] {
            run_queue.prepare_thread_slot(thread.slot() as usize);
            let sched = Arc::new(crate::ThreadSchedCell::new_test(thread, policy));
            let core = Arc::new(ThreadCore::new(
                thread, policy, sched, None, None, None, None,
            ));
            let queued = QueuedThread::new(
                thread,
                ActiveSchedulingState::new(policy, SchedulingEntity::new(policy, 1, 0)),
                core,
                false,
                false,
                RqTaskMetadata::test(1),
            );
            let _enqueue = run_queue
                .enqueue_task(queued, EnqueueReason::Wake, None)
                .expect("test RR task must enqueue");
        }

        let PickTaskResult::Continue(picked) = run_queue
            .queue
            .pick_next_task(RtEligibility::Runnable, false)
            .expect("test RR queue must have a runnable task")
        else {
            panic!("RR test setup must not select a delayed Fair task");
        };
        assert_eq!(picked.id(), current);
        let metadata = picked.metadata().clone();
        let core = Arc::clone(picked.core());
        run_queue.queue.set_next_task(&picked);
        run_queue.install_current(CurrentDispatch::new(
            CurrentDispatchState {
                thread: current,
                schedule: CurrentClassState::Linked { policy },
                metadata,
                rt_quota_exempt: false,
            },
            &core,
            RqTaskTime::test(0),
        ));
        run_queue
            .clock
            .update(RqClockSample::new(SchedulerTimestamp::from_nanos(100), 0));

        let update_request_reschedule = match run_queue
            .update_current(100, 0, 0)
            .expect("test RR runtime update must succeed")
        {
            RqCurrentUpdate::DedicatedIdle => panic!("test RR current must not become idle"),
            RqCurrentUpdate::Task { reschedule, .. } => reschedule.is_some(),
        };
        let PickTaskResult::Continue(update_next) = run_queue
            .queue
            .pick_next_task(RtEligibility::Runnable, false)
            .expect("test RR queue must remain runnable")
        else {
            panic!("RR runtime update must not create delayed Fair state");
        };

        let tick_request_reschedule = match run_queue
            .task_tick_current(0, 0, 0)
            .expect("test RR scheduler tick must succeed")
        {
            RqCurrentUpdate::DedicatedIdle => panic!("test RR current must not become idle"),
            RqCurrentUpdate::Task { reschedule, .. } => reschedule.is_some(),
        };
        let PickTaskResult::Continue(tick_next) = run_queue
            .queue
            .pick_next_task(RtEligibility::Runnable, false)
            .expect("test RR queue must remain runnable")
        else {
            panic!("RR scheduler tick must not create delayed Fair state");
        };

        (
            current,
            peer,
            update_next.id(),
            update_request_reschedule,
            tick_next.id(),
            tick_request_reschedule,
        )
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn debug_schedule_owner_count(&self, thread: ThreadId) -> usize {
        usize::from(
            self.queue.current().is_some_and(|dispatch| {
                dispatch.thread() == thread && dispatch.owns_active_schedule()
            }),
        ) + usize::from(self.queue.debug_owns_schedule_state(thread))
    }

    pub(crate) fn install_idle(
        &mut self,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        if !matches!(
            active.policy(),
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        ) {
            task_runtime::fatal_invariant(0x5251_0003, core.id().as_u64() as usize);
        }
        let idle = IdleRqTask {
            core,
            active: Some(active),
            metadata,
            rt_quota_exempt,
        };
        if self.idle.replace(idle).is_some() {
            task_runtime::fatal_invariant(0x5251_0002, self.owner.as_u32() as usize);
        }
    }

    pub(crate) fn idle(&self) -> Option<ThreadId> {
        self.idle.as_ref().map(|idle| idle.core.id())
    }

    pub(crate) fn take_idle_schedule(
        &mut self,
    ) -> Option<(Arc<ThreadCore>, ActiveSchedulingState, RqTaskMetadata, bool)> {
        let idle = self.idle.as_mut()?;
        Some((
            Arc::clone(&idle.core),
            idle.active
                .take()
                .expect("idle schedule cannot be current on two CPUs"),
            idle.metadata.clone(),
            idle.rt_quota_exempt,
        ))
    }

    pub(crate) fn return_idle_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
    ) -> Result<(), TaskError> {
        let idle = self.idle.as_mut().ok_or(TaskError::InvalidConfiguration)?;
        if idle.core.id() != thread || idle.active.replace(active).is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(())
    }

    pub(crate) fn has_exempt_rt(&self) -> bool {
        self.queue.has_exempt_rt()
    }

    pub(crate) const fn rt_is_throttled(&self) -> bool {
        self.rt_throttled
    }

    pub(crate) fn set_rt_throttled(&mut self, throttled: bool) -> bool {
        let changed = self.rt_throttled != throttled;
        self.rt_throttled = throttled;
        changed
    }

    pub(crate) fn has_runnable_rt(&self) -> bool {
        // RT current remains linked in the active priority array, so the
        // class-owned index already includes both queued and running RT work.
        self.queue.has_rt()
    }

    pub(crate) fn highest_rt_priority_including_current(&self) -> Option<u8> {
        self.highest_rt_priority()
    }

    pub(crate) fn earliest_deadline_including_current(&self) -> Option<u64> {
        // Deadline current remains linked in the augmented EDF tree.
        self.earliest_deadline_ns()
    }

    pub(crate) fn deadline_members_are_empty(&self) -> bool {
        self.queue.deadline_members_are_empty()
    }

    /// Acquires the scheduler-owned lifetime anchor for one DL hrtimer event.
    ///
    /// Linux embeds the hrtimer in `sched_dl_entity`, so the owning `task_struct`
    /// remains reachable without a process-registry lookup in hard IRQ. The
    /// per-rq Deadline member set is the equivalent lifetime authority here:
    /// every CBS/zero-lag registration is cancelled before membership leaves
    /// this rq, and the returned Arc remains valid after the rq lock is released.
    pub(crate) fn deadline_member(&self, thread: ThreadId) -> Option<Arc<ThreadCore>> {
        self.queue.deadline_member(thread)
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) -> bool {
        self.queue.register_deadline_member(core)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.queue.unregister_deadline_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .add_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.queue
            .remove_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.activate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.queue.deactivate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        self.queue.deadline_bandwidth()
    }

    /// Applies Linux EEVDF wakeup preemption to the complete owner runqueue.
    ///
    /// Dedicated idle is preempted before any class-local selection rule,
    /// matching Linux's unconditional idle-class `resched_curr()`. Otherwise,
    /// a fair wakee may request rescheduling only when it both defeats the
    /// protected current request and is itself the earliest eligible queued
    /// entity. Comparing only the wakee with current creates needless
    /// reschedule IPIs when an older queued contender would be selected.
    pub(crate) fn wakeup_preempt(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        self.wakeup_preempt_with_intent(
            wakee,
            policy,
            entity,
            fair_virtual_time,
            WakePreemptionContext::normal(),
        )
    }

    /// Applies wakeup preemption while preserving Linux wake flags.
    pub(crate) fn wakeup_preempt_with_intent(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
        context: WakePreemptionContext,
    ) -> WakePreemptionDecision {
        let Some(current) = self.current() else {
            return WakePreemptionDecision::WakeeSelected;
        };
        if current.is_dedicated_idle() {
            return WakePreemptionDecision::DedicatedIdlePreempted;
        }
        let current_policy = current.schedule_policy();
        if context.equal_rt_action == EqualRtWakeAction::RequeueWakeeAndReschedule {
            if policy.rt_priority() != current_policy.rt_priority()
                || policy.rt_priority().is_none()
                || !self.queue.requeue_realtime_wakee_head(wakee)
            {
                task_runtime::fatal_invariant(0x5251_0004, wakee.as_u64() as usize);
            }
            return WakePreemptionDecision::WakeeSelected;
        }
        let current_entity = self
            .current_scheduling_entity()
            .cloned()
            .expect("current dispatch must have one rq-owned scheduling entity");
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_wake_entity_read(wakee, 0);
        let preempts = if context.intent.is_sync() {
            crate::scheduler::default_sync_wakeup_preempts(
                current_policy,
                &current_entity,
                false,
                policy,
                entity,
                fair_virtual_time,
            )
        } else {
            crate::scheduler::wakeup_preempts(
                current_policy,
                &current_entity,
                false,
                policy,
                entity,
                fair_virtual_time,
            )
        };
        if !preempts {
            return WakePreemptionDecision::KeepCurrent;
        }
        let decision = match policy {
            SchedulePolicy::Fair { mode, .. } => {
                if self
                    .queue
                    .fair_wakee_is_selected(wakee, mode, fair_virtual_time)
                {
                    WakePreemptionDecision::WakeeSelected
                } else {
                    WakePreemptionDecision::QueuedCandidateSelected
                }
            }
            _ => WakePreemptionDecision::WakeeSelected,
        };
        if decision == WakePreemptionDecision::WakeeSelected
            && fair_preemption_cancels_protection(current_policy, &current_entity, policy, entity)
            && let Some(SchedulingEntity::Fair(current)) = self.current_scheduling_entity_mut()
        {
            current.cancel_slice_protection();
        }
        decision
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

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;
    use crate::{
        ActiveSchedulingState, CurrentClassState, CurrentDispatchState, FairMode, Nice,
        PickTaskResult, PickedThread, RqTaskMetadata, RqTaskTime, RtEligibility, SchedulingEntity,
    };

    fn fair_thread(thread: ThreadId, entity: FairEntity) -> QueuedThread {
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let sched = Arc::new(crate::ThreadSchedCell::new_test(thread, policy));
        let core = Arc::new(ThreadCore::new(
            thread, policy, sched, None, None, None, None,
        ));
        QueuedThread::new(
            thread,
            ActiveSchedulingState::new(policy, SchedulingEntity::Fair(entity)),
            core,
            false,
            true,
            RqTaskMetadata::test(1),
        )
    }

    fn install_fair_current(
        run_queue: &mut CpuRunQueueState,
        thread: ThreadId,
        entity: FairEntity,
    ) {
        run_queue.prepare_thread_slot(thread.slot() as usize);
        let _enqueue = run_queue
            .enqueue_task(fair_thread(thread, entity), EnqueueReason::Wake, None)
            .unwrap();
        let PickTaskResult::Continue(picked) = run_queue
            .queue
            .pick_next_task(RtEligibility::Runnable, false)
            .unwrap()
        else {
            panic!("test setup does not install delayed Fair sleepers")
        };
        run_queue.queue.set_next_task(&picked);
        let PickedThread::Owned(thread) = picked else {
            panic!("a Fair pick must transfer its scheduling state")
        };
        let QueuedThread {
            id,
            active,
            core,
            rt_quota_exempt,
            metadata,
            ..
        } = thread;
        let dispatch = CurrentDispatch::new(
            CurrentDispatchState {
                thread: id,
                schedule: CurrentClassState::Owned(active),
                metadata,
                rt_quota_exempt,
            },
            &core,
            RqTaskTime::test(0),
        );
        run_queue.install_current(dispatch);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn fair_put_prev_counts_the_requeued_current_once() {
        let current = ThreadId::from_parts(0, 1);
        let peer = ThreadId::from_parts(1, 1);
        let mut run_queue = CpuRunQueueState::new(CpuId::new(0), TaskSystemConfig::new(1));
        run_queue.set_virtual_time_for_test(0);
        install_fair_current(
            &mut run_queue,
            current,
            FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 0),
        );
        run_queue.prepare_thread_slot(peer.slot() as usize);
        let current_fair = run_queue
            .current_scheduling_entity()
            .and_then(|entity| entity.fair())
            .unwrap();
        let _enqueue = run_queue
            .enqueue_task(
                fair_thread(peer, FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 0)),
                EnqueueReason::Wake,
                Some(current_fair),
            )
            .unwrap();

        run_queue
            .put_prev_unlinked_current(current, EnqueueReason::Yield)
            .unwrap();

        let current_vruntime = run_queue
            .scheduling_entity(current)
            .and_then(|entity| entity.fair())
            .unwrap()
            .vruntime();
        let peer_vruntime = run_queue
            .scheduling_entity(peer)
            .and_then(|entity| entity.fair())
            .unwrap()
            .vruntime();
        assert_eq!(current_vruntime, 50);
        assert_eq!(peer_vruntime, 0);
        assert_eq!(
            run_queue.virtual_time(),
            (current_vruntime + peer_vruntime) / 2,
            "the old current snapshot must not be counted after its new entity is requeued"
        );
    }
}
