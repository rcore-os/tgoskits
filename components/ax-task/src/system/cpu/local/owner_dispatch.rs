//! Owner-only dispatch, handoff, and balance scratch facade.

use super::*;

impl CpuLocal {
    /// Returns the currently executing non-idle thread, if any.
    pub(crate) fn current(&self) -> Option<ThreadId> {
        self.remote
            .lock_run_queue(RunQueueGuardSource::OwnerCurrentThreadObservation)
            .current_thread()
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.remote
            .lock_run_queue(RunQueueGuardSource::OwnerCurrentCoreObservation)
            .current_core()
    }

    /// Returns the number of runnable non-idle threads.
    pub(crate) fn runnable_count(&self) -> usize {
        self.remote
            .lock_run_queue(RunQueueGuardSource::OwnerRunnableObservation)
            .nr_running()
    }

    /// Returns whether this owner is in a coherent idle-pull target state.
    pub(crate) fn idle_pull_eligible(&self) -> bool {
        let run_queue = self
            .remote
            .lock_run_queue(RunQueueGuardSource::OwnerRunnableObservation);
        run_queue.current_thread() == run_queue.idle() && run_queue.nr_running() == 0
    }

    pub(crate) fn is_quiescent_for_offline(&self) -> bool {
        let run_queue = self.remote.lock_run_queue(RunQueueGuardSource::Lifecycle);
        let deadlines = self
            .remote
            .read_deadline_base(DeadlineBaseGuardSource::Lifecycle);
        (run_queue.current_thread().is_none() || run_queue.current_thread() == run_queue.idle())
            && run_queue.nr_running() == 0
            && run_queue.deadline_members_are_empty()
            && deadlines.queue.is_empty()
            && deadlines.expired_count == 0
            && !deadlines.has_claimed_task_expiration()
            && !deadlines.softirq_activated
            && self.dispatch.switch_handoff.is_none()
            && self.remote.is_quiescent_for_offline()
    }

    /// Publishes a sticky reschedule request from task or IRQ context.
    pub(crate) fn request_reschedule(&self, kind: RescheduleKind) {
        self.remote.request_reschedule(kind);
    }

    pub(crate) fn request_scheduler_work(&self) {
        self.remote.request_scheduler_work();
    }

    pub(crate) fn arm_idle_pull(self: Pin<&mut Self>) {
        self.dispatch_state_mut().arm_idle_pull();
    }

    pub(crate) const fn idle_pull_pending(&self) -> bool {
        self.dispatch.idle_pull_pending()
    }

    pub(crate) fn take_idle_pull_pending(self: Pin<&mut Self>) -> bool {
        self.dispatch_state_mut().take_idle_pull_pending()
    }

    pub(crate) fn defer_scheduler_work(&self) {
        self.remote.defer_scheduler_work();
    }

    /// Tests the sticky reschedule request without clearing it.
    pub(crate) fn needs_reschedule(&self) -> bool {
        self.remote.needs_reschedule()
    }

    pub(crate) fn scheduler_request_pending(&self, scope: SchedulerRequestScope) -> bool {
        self.remote.scheduler_request_pending(scope)
    }

    pub(crate) fn promote_lazy_reschedule(&self) -> bool {
        self.remote.promote_lazy_reschedule()
    }

    /// Returns the bounded scheduler safe-point work budget.
    pub(crate) const fn batch_limit(&self) -> usize {
        self.drain.batch_limit()
    }

    /// Reads current lifecycle under an already-active scheduler IRQ-off baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must keep local IRQs disabled through this read.
    pub(crate) unsafe fn scheduler_current_lifecycle_state(&self) -> Option<ThreadState> {
        // SAFETY: forwarded from this method's scheduler-frame contract.
        unsafe { self.remote.lock_run_queue_irq_disabled() }
            .current()
            .map(|dispatch| dispatch.runtime_core().state())
    }

    /// Installs the dedicated idle task during offline CPU bootstrap.
    ///
    /// # Safety
    ///
    /// The caller must retain the boot CPU's raw IRQ exclusion and
    /// `PREEMPT_DISABLED` ownership for the complete operation.
    pub(crate) unsafe fn install_idle_bootstrap(
        self: Pin<&mut Self>,
        system: &TaskSystem,
        idle: ThreadId,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        debug_assert_eq!(idle, core.id());
        // SAFETY: changing fields does not move this pinned object.
        let fields = unsafe { self.get_unchecked_mut() };
        // SAFETY: forwarded from this method's offline boot-owner contract.
        let mut transaction = unsafe { OwnerRqTxn::begin_bootstrap(system, &fields.remote) };
        transaction.install_idle(Arc::clone(&core), active, metadata, rt_quota_exempt);
        core.sched().placement().install_idle(fields.owner);
        transaction.commit_bootstrap();
        fields.remote.publish_idle_thread(idle);
    }

    pub(crate) fn stage_switch_handoff(
        self: Pin<&mut Self>,
        previous: Arc<ThreadCore>,
        incoming: Arc<ThreadCore>,
        migration: Option<PreparedMigrationDelivery>,
    ) -> Result<(), TaskError> {
        let handoff = &mut self.dispatch_state_mut().switch_handoff;
        if handoff.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        *handoff = Some(SwitchHandoff::prepared(previous, incoming, migration));
        Ok(())
    }

    pub(crate) fn install_switch_rq_baton(
        self: Pin<&mut Self>,
        baton: RqSwitchBaton,
    ) -> Result<(), TaskError> {
        if baton.owner() != self.owner() {
            return Err(TaskError::InvalidConfiguration);
        }
        self.dispatch_state_mut()
            .switch_handoff
            .as_mut()
            .ok_or(TaskError::InvalidConfiguration)?
            .install_rq_baton(baton)
    }

    pub(crate) fn finish_switch_rq_baton(
        self: Pin<&mut Self>,
        previous: ThreadId,
    ) -> Result<bool, TaskError> {
        let owner = self.owner();
        let handoff = self
            .dispatch_state_mut()
            .switch_handoff
            .as_mut()
            .ok_or(TaskError::InvalidConfiguration)?;
        if handoff.previous().id() != previous {
            return Err(TaskError::InvalidConfiguration);
        }
        let Some(baton) = handoff.take_rq_baton() else {
            return Ok(false);
        };
        baton.finish(owner)?;
        Ok(true)
    }

    pub(crate) fn finish_switch_runtime_tail(
        mut self: Pin<&mut Self>,
        previous: ThreadId,
        migration_target: Option<CpuId>,
        reclaim_ready: bool,
    ) -> Result<(), TaskError> {
        let handoff = self
            .as_mut()
            .dispatch_state_mut()
            .switch_handoff
            .take()
            .ok_or(TaskError::InvalidConfiguration)?;
        if handoff.previous().id() != previous
            || handoff.migration_target() != migration_target
            || handoff.runtime_tail_is_finished()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.dispatch_state_mut().switch_handoff =
            Some(handoff.finish_runtime_tail(reclaim_ready)?);
        Ok(())
    }

    pub(crate) fn take_switch_handoff(self: Pin<&mut Self>) -> Option<SwitchHandoff> {
        self.dispatch_state_mut().switch_handoff.take()
    }

    pub(crate) fn switch_handoff(&self) -> Option<&SwitchHandoff> {
        self.dispatch.switch_handoff.as_ref()
    }

    pub(crate) fn defer_park_preemption(&self, request: SchedulerRequestClaim) {
        self.remote.defer_park_preemption(request);
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        self.remote.finish_park_preemption(resume_running);
    }

    pub(crate) fn dispatch_state_mut(
        self: Pin<&mut Self>,
    ) -> &mut dispatch_state::OwnerDispatchState {
        // SAFETY: the owner borrow is pinned, and OwnerDispatchState contains
        // no self-referential pointer that can move CpuLocal.
        &mut unsafe { self.get_unchecked_mut() }.dispatch
    }

    pub(crate) fn lock_run_queue(
        &self,
        source: RunQueueGuardSource,
    ) -> IrqTicketGuard<'_, CpuRunQueueState> {
        self.remote.lock_run_queue(source)
    }

    pub(crate) fn drain_state_mut(self: Pin<&mut Self>) -> &mut drain_state::OwnerDrainScratch {
        // SAFETY: scratch buffers are owner-only and do not move CpuLocal.
        &mut unsafe { self.get_unchecked_mut() }.drain
    }

    pub(crate) const fn drain_state(&self) -> &drain_state::OwnerDrainScratch {
        &self.drain
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn deadline_members_are_empty_for_test(&self) -> bool {
        self.remote
            .lock_run_queue(RunQueueGuardSource::DeadlineAccounting)
            .deadline_members_are_empty()
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        self.remote.balance_request_node()
    }

    pub(crate) const fn idle_pull_visited(&self) -> &CpuSet {
        self.dispatch.idle_pull_visited()
    }

    pub(crate) fn mark_idle_pull_source(self: Pin<&mut Self>, source: CpuId) {
        self.dispatch_state_mut().mark_idle_pull_source(source);
    }

    pub(crate) fn reset_idle_pull_scan(self: Pin<&mut Self>) {
        self.dispatch_state_mut().reset_idle_pull_scan();
    }
}
