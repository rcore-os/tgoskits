//! Linux-style owner runqueue transaction.

use super::*;
use crate::{
    BalanceScan, EnqueueReason, FairEntity, PickedThread, QueuedThreadSnapshot, RtEligibility,
    system::task_system::{SwitchEndpoint, TaskSystem},
};

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

    pub(crate) fn begin(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        let mut run_queue = remote.lock_run_queue();
        let clock = run_queue.update_clock();
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

    pub(crate) fn current_scheduling_entity(&self) -> Option<SchedulingEntity> {
        self.run_queue().current_scheduling_entity()
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
        &self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        self.run_queue()
            .wakeup_preempt(wakee, policy, entity, fair_virtual_time)
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

    pub(crate) fn enqueue_task(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> SchedulingEntity {
        let id = thread.id;
        self.scheduler_queue_mut()
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

    pub(crate) fn pick_next_task(&mut self, rt_eligibility: RtEligibility) -> Option<PickedThread> {
        self.scheduler_queue_mut().pick_next_task(rt_eligibility)
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
        let accounting = self
            .run_queue_mut()
            .task_tick_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        match accounting {
            RqCurrentTick::DedicatedIdle => DispatchCharge::default(),
            RqCurrentTick::Task {
                charge,
                request_reschedule,
                realtime,
                rt_quota_exempt,
            } => {
                let rt_throttled = realtime
                    && self
                        .system
                        .charge_rt_runtime(self.remote.owner(), runtime_ns);
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

    /// Commits the rq state before acknowledging the scheduler request epoch.
    ///
    /// Only a claim explicitly adopted or merged before the scheduler made its
    /// decision belongs to this acknowledgement. A request published after
    /// that decision remains a newer generation for the next scheduler pass.
    pub(crate) fn commit_and_acknowledge_scheduler_request(mut self) {
        let claim = self
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
        remote.acknowledge_scheduler_request(claim);
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
