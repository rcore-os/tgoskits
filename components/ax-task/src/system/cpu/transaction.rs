//! Linux-style owner runqueue transaction.

use super::*;
use crate::{
    BalanceScan, EnqueueReason, FairEntity, PickTaskResult, PickedThread, QueuedThreadSnapshot,
    RtEligibility,
    system::{
        task_system::{SwitchEndpoint, TaskSystem},
        thread_sched::{SchedulerPlacement, ThreadSchedCell, ThreadSchedState},
    },
};

/// Linux `task_current()` and `task_on_rq_queued()` facts sampled under one rq
/// lock.
///
/// `Queued { outgoing: true }` is the legal switch-handoff window where the
/// task has left `rq->curr` but still retains its `p->on_cpu` stack claim.
/// `DelayedFair` keeps Linux's distinct `on_rq && sched_delayed` state from
/// being mistaken for runnable queue membership by policy and PI updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::system) enum OwnerRqTaskState {
    Current,
    Queued { outgoing: bool },
    DelayedFair { outgoing: bool },
    Inactive,
}

impl OwnerRqTaskState {
    pub(in crate::system) const fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }

    pub(in crate::system) const fn is_queued(self) -> bool {
        matches!(self, Self::Queued { .. })
    }

    pub(in crate::system) const fn is_runnable(self) -> bool {
        matches!(self, Self::Current | Self::Queued { .. })
    }

    pub(in crate::system) const fn is_delayed_fair(self) -> bool {
        matches!(self, Self::DelayedFair { .. })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum OwnerRqEntry {
    IrqSave,
    SchedulerFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerRqContext {
    RuntimeIrqSave,
    SchedulerFrame,
    OfflineBootstrap,
}

impl OwnerRqEntry {
    /// Locks task scheduler state under this rq entry's IRQ ownership model.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    pub(in crate::system) unsafe fn lock_thread_sched(
        self,
        cell: &ThreadSchedCell,
    ) -> IrqTicketGuard<'_, ThreadSchedState> {
        match self {
            Self::IrqSave => cell.lock(),
            Self::SchedulerFrame => {
                // SAFETY: forwarded from this method's contract.
                unsafe { cell.lock_scheduler_frame() }
            }
        }
    }

    /// Begins the selected rq locking protocol.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    pub(crate) unsafe fn begin<'a>(
        self,
        system: &'a TaskSystem,
        remote: &'a CpuRemote,
    ) -> OwnerRqTxn<'a> {
        match self {
            Self::IrqSave => OwnerRqTxn::begin(system, remote),
            Self::SchedulerFrame => {
                // SAFETY: forwarded from this method's contract.
                unsafe { OwnerRqTxn::begin_scheduler(system, remote) }
            }
        }
    }
}

/// One owner-CPU runqueue critical section.
///
/// Construction disables local IRQs, locks the rq, and samples `rq->clock`
/// exactly once. Callers must use the retained wall/task pair for the complete
/// class transition instead of opening nested rq locks or sampling a second
/// clock value.
pub(crate) struct OwnerRqTxn<'a> {
    system: &'a TaskSystem,
    remote: &'a CpuRemote,
    run_queue: Option<IrqTicketGuard<'a, CpuRunQueueState>>,
    clock: RunQueueClockSnapshot,
    request: Option<SchedulerRequestClaim>,
    context: OwnerRqContext,
    finished: bool,
}

/// Raw owner-rq lock ownership inherited by an incoming switch tail.
///
/// Linux carries `rq->lock` through `switch_to()` and releases it only after
/// `finish_task(prev)` clears `prev->on_cpu`. This token represents the same
/// bounded ownership interval without retaining mutable access to rq state.
#[derive(Debug)]
pub(crate) struct RqSwitchBaton {
    owner: CpuId,
    _raw: RawTicketBaton<CpuRunQueueState>,
}

impl RqSwitchBaton {
    pub(crate) const fn owner(&self) -> CpuId {
        self.owner
    }

    pub(crate) fn finish(self, owner: CpuId) -> Result<(), TaskError> {
        if self.owner != owner {
            return Err(TaskError::InvalidConfiguration);
        }
        drop(self);
        Ok(())
    }
}

impl<'a> OwnerRqTxn<'a> {
    fn run_queue(&self) -> &CpuRunQueueState {
        self.run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock")
    }

    fn run_queue_mut(&mut self) -> &mut CpuRunQueueState {
        self.run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock")
    }

    fn scheduler_queue_mut(&mut self) -> &mut RunQueue {
        self.run_queue_mut().owner_transaction_queue_mut()
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn owns_runtime_irq_scope(&self) -> bool {
        self.run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock")
            .owns_runtime_irq_scope()
    }

    pub(crate) fn begin(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        let mut run_queue = remote.lock_run_queue(RunQueueGuardSource::Transaction);
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_owner_rq_irqsave_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::RuntimeIrqSave,
            finished: false,
        }
    }

    /// Begins an rq transaction below a task scheduler lock that already owns
    /// local IRQ exclusion.
    pub(crate) fn begin_nested(
        system: &'a TaskSystem,
        remote: &'a CpuRemote,
        irq_owner: &'a IrqOwner<'_>,
    ) -> Self {
        let mut run_queue = remote.lock_run_queue_nested(irq_owner);
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_owner_rq_irqsave_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::RuntimeIrqSave,
            finished: false,
        }
    }

    /// Begins the transaction from `__schedule()`/IRQ-return context where the
    /// runtime scheduler frame already owns local IRQ exclusion.
    ///
    /// # Safety
    ///
    /// The scheduler IRQ-off baton must outlive this transaction.
    pub(crate) unsafe fn begin_scheduler(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        // SAFETY: forwarded from this constructor's contract.
        let mut run_queue = unsafe { remote.lock_run_queue_irq_disabled() };
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_owner_rq_scheduler_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::SchedulerFrame,
            finished: false,
        }
    }

    /// Begins the first rq transaction while its owner CPU is still offline.
    ///
    /// This is the `sched_init()` counterpart of [`Self::begin_scheduler`]:
    /// boot already owns raw IRQ exclusion and `PREEMPT_DISABLED`, but no
    /// runtime IRQ-exit service may run until rq/current/idle are published.
    ///
    /// # Safety
    ///
    /// The calling CPU must retain its offline boot ownership and local IRQs
    /// must remain disabled for the complete transaction.
    pub(crate) unsafe fn begin_bootstrap(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        // SAFETY: forwarded from this constructor's boot-owner contract.
        let mut run_queue = unsafe { remote.lock_run_queue_irq_disabled() };
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_owner_rq_bootstrap_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::OfflineBootstrap,
            finished: false,
        }
    }

    pub(crate) const fn clock(&self) -> RunQueueClockSnapshot {
        self.clock
    }

    pub(crate) const fn owner(&self) -> CpuId {
        self.remote.owner()
    }

    pub(crate) fn scheduler_deadline_rq_observation(
        &self,
        cpu: &CpuLocal,
    ) -> SchedulerDeadlineRqObservation {
        assert_eq!(
            cpu.owner(),
            self.owner(),
            "scheduler deadline observation must use its owner CPU transaction"
        );
        cpu.scheduler_deadline_rq_observation_in_run_queue(self.run_queue())
    }

    pub(crate) fn claim_scheduler_request(&mut self) -> SchedulerRequestClaim {
        let claim = self.remote.claim_scheduler_request();
        self.request = Some(self.request.map_or(claim, |current| current.merge(claim)));
        self.request
            .expect("scheduler request claim was just installed")
    }

    pub(crate) fn adopt_scheduler_request(&mut self, claim: SchedulerRequestClaim) {
        assert!(
            self.request.replace(claim).is_none(),
            "one rq transaction may adopt only one initial scheduler claim"
        );
    }

    pub(crate) fn merge_scheduler_request(&mut self) -> SchedulerRequestClaim {
        self.claim_scheduler_request()
    }

    pub(crate) fn current(&self) -> Option<&CurrentDispatch> {
        self.run_queue().current()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.run_queue_mut().current_mut()
    }

    pub(crate) fn current_scheduling_entity(&self) -> Option<&SchedulingEntity> {
        self.run_queue().current_scheduling_entity()
    }

    pub(crate) fn current_fair_contender(&self) -> Option<FairEntity> {
        self.run_queue().current_fair_contender()
    }

    pub(crate) fn current_scheduling_entity_mut(&mut self) -> Option<&mut SchedulingEntity> {
        self.run_queue_mut().current_scheduling_entity_mut()
    }

    pub(crate) fn linked_current_entity_mut(
        &mut self,
        thread: ThreadId,
    ) -> Option<&mut SchedulingEntity> {
        self.run_queue_mut().linked_current_entity_mut(thread)
    }

    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        self.scheduler_queue_mut().update_fair_virtual_time(current);
    }

    pub(crate) fn wakeup_preempt(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        self.run_queue_mut()
            .wakeup_preempt(wakee, policy, entity, fair_virtual_time)
    }

    pub(crate) fn wakeup_preempt_with_intent(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
        context: WakePreemptionContext,
    ) -> WakePreemptionDecision {
        self.run_queue_mut().wakeup_preempt_with_intent(
            wakee,
            policy,
            entity,
            fair_virtual_time,
            context,
        )
    }

    pub(crate) fn capture_current_fair_migration(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) {
        self.run_queue_mut()
            .capture_current_fair_migration(thread, timing_granularity_ns);
    }

    pub(crate) fn current_thread(&self) -> Option<ThreadId> {
        self.run_queue().current_thread()
    }

    /// Samples the task's Linux rq facts while this transaction owns the rq.
    pub(in crate::system) fn task_state(
        &self,
        thread: ThreadId,
        placement: &SchedulerPlacement,
    ) -> OwnerRqTaskState {
        let owner = self.owner();
        let queued = placement.queued_cpu() == Some(owner);
        let on_cpu = placement.on_cpu() == Some(owner);
        if self.current_thread() == Some(thread) {
            if !queued || !on_cpu {
                task_runtime::fatal_invariant(0x5251_1011, thread.as_u64() as usize);
            }
            OwnerRqTaskState::Current
        } else if queued && self.is_delayed_fair(thread) {
            OwnerRqTaskState::DelayedFair { outgoing: on_cpu }
        } else if queued {
            OwnerRqTaskState::Queued { outgoing: on_cpu }
        } else {
            OwnerRqTaskState::Inactive
        }
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.run_queue().current_core()
    }

    pub(crate) fn current_switch_endpoint(&self) -> Option<SwitchEndpoint> {
        self.run_queue()
            .current()
            .map(CurrentDispatch::switch_endpoint)
    }

    pub(crate) fn update_current_runtime_binding(
        &mut self,
        thread: ThreadId,
        binding: crate::runtime::ThreadRuntimeBinding,
    ) {
        self.run_queue_mut()
            .update_current_runtime_binding(thread, binding)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1003, thread.as_u64() as usize)
            });
    }

    pub(crate) fn refresh_current_scheduler_metadata(
        &mut self,
        thread: ThreadId,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        self.run_queue_mut()
            .refresh_current_scheduler_metadata(thread, metadata, rt_quota_exempt);
    }

    /// Linux-style rq mutation: placement was validated under `p->pi_lock`
    /// before the owner rq transaction began, so a missing entity here is an
    /// ownership violation rather than a recoverable scheduling result.
    pub(crate) fn deactivate_task(&mut self, thread: ThreadId) -> QueuedThread {
        self.scheduler_queue_mut()
            .deactivate_task(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1007, thread.as_u64() as usize))
    }

    pub(crate) fn deactivate_unlinked_current(&mut self, thread: ThreadId) {
        self.scheduler_queue_mut()
            .deactivate_unlinked_current(thread);
    }

    pub(crate) fn delay_dequeue_unlinked_current(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
        force: bool,
    ) -> Option<SchedulingEntity> {
        self.run_queue_mut()
            .delay_dequeue_unlinked_current(thread, timing_granularity_ns, force)
    }

    pub(crate) fn is_delayed_fair(&self, thread: ThreadId) -> bool {
        self.run_queue().is_delayed_fair(thread)
    }

    pub(crate) fn finish_delayed_fair_dequeue(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) -> QueuedThread {
        self.run_queue_mut()
            .finish_delayed_fair_dequeue(thread, timing_granularity_ns)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1014, thread.as_u64() as usize))
    }

    pub(crate) fn reactivate_delayed_fair(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) -> OwnerRqEnqueue {
        self.run_queue_mut()
            .reactivate_delayed_fair(thread, timing_granularity_ns)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1015, thread.as_u64() as usize))
    }

    pub(crate) fn throttle_current_deadline(
        &mut self,
        thread: ThreadId,
    ) -> Result<SchedulingEntity, TaskError> {
        self.scheduler_queue_mut().throttle_current_deadline(thread)
    }

    pub(crate) fn replenish_throttled_deadline(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> Result<(), TaskError> {
        self.scheduler_queue_mut()
            .replenish_throttled_deadline(thread, entity)
    }

    /// Unlinks one runnable entity for a class change without changing
    /// `rq->nr_running`.
    pub(crate) fn reclassify_task(&mut self, thread: ThreadId) -> QueuedThread {
        self.scheduler_queue_mut()
            .reclassify_task(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1008, thread.as_u64() as usize))
    }

    pub(crate) fn take_delayed_fair_for_update(&mut self, thread: ThreadId) -> QueuedThread {
        self.run_queue_mut()
            .take_delayed_fair_for_update(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1016, thread.as_u64() as usize))
    }

    pub(crate) fn restore_delayed_fair_after_update(
        &mut self,
        thread: QueuedThread,
    ) -> SchedulingEntity {
        self.run_queue_mut()
            .restore_delayed_fair_after_update(thread)
    }

    pub(crate) fn finish_detached_delayed_fair(
        &mut self,
        active: &mut ActiveSchedulingState,
        timing_granularity_ns: u64,
    ) {
        self.run_queue_mut()
            .finish_detached_delayed_fair(active, timing_granularity_ns);
    }

    pub(crate) fn enqueue_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_delayed_fair_transfer(thread, current_fair)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1017, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_reactivated_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_reactivated_delayed_fair_transfer(thread, current_fair, timing_granularity_ns)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1018, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_task(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_task(thread, reason, current_fair)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1006, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_throttled_deadline(&mut self, thread: QueuedThread) {
        let id = thread.id;
        self.scheduler_queue_mut()
            .enqueue_throttled_deadline(thread)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_100d, id.as_u64() as usize));
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        if !self.run_queue_mut().register_deadline_member(core) {
            task_runtime::fatal_invariant(0x5251_100e, core.id().as_u64() as usize);
        }
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.run_queue_mut().unregister_deadline_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.run_queue_mut()
            .add_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.run_queue_mut()
            .remove_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.run_queue_mut()
            .activate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.run_queue_mut()
            .deactivate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn update_base_deadline_entity(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        self.run_queue_mut()
            .update_base_deadline_entity(thread, entity)
    }

    pub(crate) fn begin_balance_scan(&mut self, class: Option<SchedulingClass>) -> BalanceScan {
        self.scheduler_queue_mut().begin_balance_scan(class)
    }

    pub(crate) fn next_balance_candidate(
        &mut self,
        scan: &mut BalanceScan,
        may_migrate: impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        self.scheduler_queue_mut()
            .next_balance_candidate(scan, may_migrate)
    }

    pub(crate) fn detach_for_transfer(
        &mut self,
        thread: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        self.scheduler_queue_mut()
            .detach_for_transfer(thread, current_fair, timing_granularity_ns)
    }

    pub(crate) fn pick_next_task(
        &mut self,
        rt_eligibility: RtEligibility,
        skip_delayed: bool,
    ) -> Option<PickTaskResult> {
        self.scheduler_queue_mut()
            .pick_next_task(rt_eligibility, skip_delayed)
    }

    pub(crate) fn rollback_pick(&mut self, picked: PickedThread) {
        self.scheduler_queue_mut().rollback_pick(picked);
    }

    pub(crate) fn set_next_task(&mut self, picked: &PickedThread) {
        self.scheduler_queue_mut().set_next_task(picked);
    }

    pub(crate) fn update_migration_capability(
        &mut self,
        thread: ThreadId,
        migration_capable: bool,
    ) {
        if !self
            .scheduler_queue_mut()
            .update_migration_capability(thread, migration_capable)
        {
            task_runtime::fatal_invariant(0x5251_1009, thread.as_u64() as usize);
        }
    }

    pub(crate) fn idle(&self) -> Option<ThreadId> {
        self.run_queue().idle()
    }

    pub(crate) fn take_idle_schedule(
        &mut self,
    ) -> Option<(Arc<ThreadCore>, ActiveSchedulingState, RqTaskMetadata, bool)> {
        self.run_queue_mut().take_idle_schedule()
    }

    pub(crate) fn return_idle_schedule(&mut self, thread: ThreadId, active: ActiveSchedulingState) {
        self.run_queue_mut()
            .return_idle_schedule(thread, active)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_100a, thread.as_u64() as usize)
            });
    }

    pub(crate) fn install_idle(
        &mut self,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        self.run_queue_mut()
            .install_idle(core, active, metadata, rt_quota_exempt);
    }

    pub(crate) fn take_current(&mut self) -> Option<CurrentDispatch> {
        #[cfg(feature = "task-test-hooks")]
        if let Some(thread) = self.current_thread() {
            crate::task_test_hooks::record_current_dispatch_detach(thread);
        }
        self.run_queue_mut().take_current()
    }

    pub(crate) fn detach_current_schedule(&mut self, thread: ThreadId) -> ActiveSchedulingState {
        self.run_queue_mut()
            .detach_current_schedule(thread)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1004, thread.as_u64() as usize)
            })
    }

    pub(crate) fn install_current_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
        core: Arc<ThreadCore>,
        rt_quota_exempt: bool,
        migration_capable: bool,
        metadata: RqTaskMetadata,
    ) {
        self.run_queue_mut()
            .install_current_schedule(
                thread,
                active,
                core,
                rt_quota_exempt,
                migration_capable,
                metadata,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1005, thread.as_u64() as usize)
            });
    }

    pub(crate) fn put_prev_task(
        &mut self,
        thread: ThreadId,
        reason: EnqueueReason,
    ) -> SchedulingEntity {
        self.scheduler_queue_mut()
            .put_prev_task(thread, reason)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_100b, thread.as_u64() as usize)
            })
    }

    pub(crate) fn put_prev_unlinked_current(
        &mut self,
        thread: ThreadId,
        reason: EnqueueReason,
    ) -> SchedulingEntity {
        self.run_queue_mut()
            .put_prev_unlinked_current(thread, reason)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1010, thread.as_u64() as usize)
            })
    }

    pub(crate) fn install_current(&mut self, dispatch: CurrentDispatch) {
        let role = if self.run_queue().idle() == Some(dispatch.thread()) {
            DispatchRole::DedicatedIdle
        } else {
            DispatchRole::Task
        };
        self.run_queue_mut()
            .install_current(dispatch.with_role(role));
    }

    pub(crate) fn charge_current(&mut self, runtime_ns: u64, reclaimed_ns: u64) -> DispatchCharge {
        let deadline_extra_bw_scaled = self.remote.deadline_extra_bw_scaled();
        let update = self
            .run_queue_mut()
            .update_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    pub(crate) fn task_tick_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        let deadline_extra_bw_scaled = self.remote.deadline_extra_bw_scaled();
        let update = self
            .run_queue_mut()
            .task_tick_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    fn apply_current_update(&mut self, runtime_ns: u64, update: RqCurrentUpdate) -> DispatchCharge {
        match update {
            RqCurrentUpdate::DedicatedIdle => DispatchCharge::default(),
            RqCurrentUpdate::Task {
                charge,
                request_reschedule,
                realtime,
                rt_quota_exempt,
            } => {
                let already_throttled = self.run_queue().rt_is_throttled();
                let rt_throttled = realtime
                    && self.system.rt_bandwidth_enabled()
                    && self.system.charge_rt_runtime(
                        self.remote.owner(),
                        runtime_ns,
                        already_throttled,
                    );
                if rt_throttled {
                    self.run_queue_mut().set_rt_throttled(true);
                }
                self.remote.charge_busy_runtime(runtime_ns);
                if request_reschedule || (rt_throttled && !rt_quota_exempt) {
                    self.remote.request_reschedule();
                }
                charge
            }
        }
    }

    pub(crate) fn rt_is_effectively_throttled(&self) -> bool {
        self.run_queue().rt_is_throttled() && !self.run_queue().has_exempt_rt()
    }

    pub(crate) fn rt_is_throttled(&self) -> bool {
        self.run_queue().rt_is_throttled()
    }

    pub(crate) fn set_rt_throttled(&mut self, throttled: bool) -> bool {
        self.run_queue_mut().set_rt_throttled(throttled)
    }

    pub(crate) fn settle_current(&mut self, reclaimed_ns: u64) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        self.charge_current(runtime_ns, reclaimed_ns)
    }

    pub(crate) fn task_tick_current_until(&mut self, reclaimed_ns: u64) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        self.task_tick_current(runtime_ns, reclaimed_ns)
    }

    /// Commits every rq-derived publication exactly once and releases the rq.
    ///
    /// This is the ax-task equivalent of leaving one Linux rq-lock
    /// transaction after `put_prev_task()`/`pick_next_task()`/`set_next_task()`
    /// and updating cpupri/cpudl/overload from that final state. Publication is
    /// explicit rather than a `Drop` fallback so a partial transition cannot
    /// become externally visible by accident.
    pub(crate) fn commit(mut self) {
        if self.context == OwnerRqContext::OfflineBootstrap {
            task_runtime::fatal_invariant(0x5251_100f, self.remote.owner().as_u32() as usize);
        }
        let run_queue = self
            .run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock");
        self.system
            .publish_run_queue_summary(self.remote, run_queue);
        self.finished = true;
        drop(self.run_queue.take());
    }

    /// Commits owner-local bootstrap state without publishing an offline rq
    /// into the root-domain priority indexes.
    ///
    /// Linux initializes `rq`, `curr`, and `idle` while the CPU is offline;
    /// cpupri/cpudl publication starts only when the rq joins the online root
    /// domain. Keeping the phases separate also prevents `sched_init()` from
    /// entering the runtime IRQ-exit service through nested index locks.
    pub(crate) fn commit_bootstrap(mut self) {
        if self.context != OwnerRqContext::OfflineBootstrap || self.remote.is_online() {
            task_runtime::fatal_invariant(0x5251_1010, self.remote.owner().as_u32() as usize);
        }
        let run_queue = self
            .run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock");
        let _ = self.remote.publish_run_queue_load_summary(run_queue);
        self.finished = true;
        drop(self.run_queue.take());
    }

    /// Commits the rq state before the final scheduler-work recheck.
    ///
    /// A request published after the decision sets a sticky entry bit for the
    /// next pass. Owner-inbox work that remains after this transaction is
    /// explicitly rearmed after the rq state becomes visible.
    pub(crate) fn commit_and_finish_scheduler_request(mut self) {
        let _claim = self
            .request
            .take()
            .expect("a scheduler rq transaction must merge its decision claim");
        let remote = self.remote;
        let run_queue = self
            .run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock");
        self.system.publish_run_queue_summary(remote, run_queue);
        self.finished = true;
        drop(self.run_queue.take());
        remote.finish_scheduler_request();
    }

    /// Publishes the selected rq state but transfers its raw lock to switch
    /// tail instead of releasing it in the outgoing scheduler context.
    ///
    /// Scheduler work is rechecked while rq remains locked. A concurrent
    /// publication leaves its sticky bit set for the next pass, while the
    /// selected switch cannot race the physical handoff.
    pub(crate) fn commit_and_handoff_scheduler_work(mut self) -> RqSwitchBaton {
        if self.context != OwnerRqContext::SchedulerFrame {
            task_runtime::fatal_invariant(0x5251_1011, self.remote.owner().as_u32() as usize);
        }
        let _claim = self
            .request
            .take()
            .expect("a scheduler rq transaction must merge its decision claim");
        let remote = self.remote;
        let run_queue = self
            .run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock");
        self.system.publish_run_queue_summary(remote, run_queue);
        self.finished = true;
        let guard = self
            .run_queue
            .take()
            .expect("an unfinished rq transaction must retain its lock");
        remote.finish_scheduler_request();
        // SAFETY: scheduler-frame construction guarantees that this guard has
        // no owned irqsave scope. CpuLocal retains both the lock allocation and
        // the outer IRQ-off scheduler baton until switch-tail completion.
        let raw = unsafe { guard.into_raw_baton() };
        RqSwitchBaton {
            owner: remote.owner(),
            _raw: raw,
        }
    }
}

impl Deref for OwnerRqTxn<'_> {
    type Target = CpuRunQueueState;

    fn deref(&self) -> &Self::Target {
        self.run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock")
    }
}

impl Drop for OwnerRqTxn<'_> {
    fn drop(&mut self) {
        if !self.finished {
            task_runtime::fatal_invariant(0x5251_5458, self.remote.owner().as_u32() as usize);
        }
    }
}
