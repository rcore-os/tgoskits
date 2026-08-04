use super::*;

mod deadline_state;
mod dispatch_state;
mod drain_state;

use deadline_state::TaskDeadlinePublicationState;

/// Scheduler state that is created explicitly and mutated only by its owner CPU.
///
/// The object is `!Unpin`; runtimes store it in per-CPU pinned allocations and
/// publish it only after registration has completed.
#[derive(Debug)]
pub struct CpuLocal {
    owner: CpuId,
    remote: Arc<CpuRemote>,
    dispatch: dispatch_state::OwnerDispatchState,
    task_deadlines: deadline_state::LocalTaskDeadlineState,
    drain: drain_state::OwnerDrainScratch,
    _pinned: PhantomPinned,
}

impl CpuLocal {
    pub(crate) fn create(
        owner: CpuId,
        config: TaskSystemConfig,
        remote: Arc<CpuRemote>,
    ) -> Pin<Box<Self>> {
        debug_assert_eq!(owner, remote.owner());
        Box::pin(Self {
            owner,
            remote,
            dispatch: dispatch_state::OwnerDispatchState::new(config),
            task_deadlines: deadline_state::LocalTaskDeadlineState::new(config),
            drain: drain_state::OwnerDrainScratch::new(config),
            _pinned: PhantomPinned,
        })
    }

    /// Returns the logical processor that exclusively owns the run queue.
    pub const fn owner(&self) -> CpuId {
        self.owner
    }

    /// Returns whether registration and online publication have completed.
    pub fn is_online(&self) -> bool {
        self.remote.is_online()
    }

    pub(crate) fn remote(&self) -> &Arc<CpuRemote> {
        &self.remote
    }

    /// Returns the currently executing non-idle thread, if any.
    pub const fn current(&self) -> Option<ThreadId> {
        self.dispatch.current
    }

    pub(crate) fn current_core(&self) -> Option<&Arc<ThreadCore>> {
        self.dispatch.current_core.as_ref()
    }

    /// Clones a strong handle for the currently executing thread.
    ///
    /// This owner-side lookup never consults the generation registry. The
    /// stable core retained by `CpuLocal` pins the registry record and any OS
    /// extension until the returned handle is dropped.
    pub fn current_thread_handle(&self) -> Result<ThreadHandle, TaskError> {
        self.dispatch
            .current_core
            .as_ref()
            .map(|core| ThreadHandle::from_core(Arc::clone(core)))
            .ok_or(TaskError::NoRunnableThread)
    }

    /// Returns the configured CPU idle thread, if any.
    pub const fn idle(&self) -> Option<ThreadId> {
        self.dispatch.idle
    }

    /// Returns the number of runnable non-idle threads.
    pub(crate) fn runnable_count(&self) -> usize {
        self.remote.lock_run_queue().len()
    }

    pub(crate) fn is_quiescent_for_offline(&self) -> bool {
        let run_queue = self.remote.lock_run_queue();
        (self.dispatch.current.is_none() || self.dispatch.current == self.dispatch.idle)
            && run_queue.len() == 0
            && run_queue.deadline_members_are_empty()
            && self.task_deadlines.queue.is_empty()
            && self.task_deadlines.expired_count == 0
            && self.dispatch.switch_handoff.is_none()
            && self.remote.is_quiescent_for_offline()
    }

    /// Publishes a sticky reschedule request from task or IRQ context.
    pub fn request_reschedule(&self) {
        self.remote.request_reschedule();
    }

    pub(crate) fn request_scheduler_work(&self) {
        self.remote.request_scheduler_work();
    }

    pub(crate) fn defer_scheduler_work(&self) {
        self.remote.defer_scheduler_work();
    }

    /// Tests the sticky reschedule request without clearing it.
    pub fn needs_reschedule(&self) -> bool {
        self.remote.needs_reschedule()
    }

    /// Returns the preallocated task-deadline capacity selected at construction.
    pub fn timer_capacity(&self) -> usize {
        self.task_deadlines.queue.capacity()
    }

    /// Returns the bounded scheduler safe-point work budget.
    pub const fn batch_limit(&self) -> usize {
        self.drain.batch_limit()
    }

    pub(crate) fn clear_current(self: Pin<&mut Self>) {
        // SAFETY: the scheduler owns this pinned object; projecting disjoint
        // fields does not move the CpuLocal identity.
        let this = unsafe { self.get_unchecked_mut() };
        this.dispatch.current = None;
        this.dispatch.current_core = None;
        this.dispatch.current_dispatch = None;
        this.remote.publish_current_thread(None);
    }

    pub(crate) fn set_current_core(self: Pin<&mut Self>, core: Arc<ThreadCore>) {
        let id = core.id();
        // SAFETY: the scheduler owns this pinned object; projecting disjoint
        // fields does not move the CpuLocal identity.
        let this = unsafe { self.get_unchecked_mut() };
        this.dispatch.current = Some(id);
        this.dispatch.current_core = Some(core);
        this.remote.publish_current_thread(Some(id));
        this.remote.mark_scheduler_ready();
    }

    pub(crate) fn install_dispatch(self: Pin<&mut Self>, dispatch: CurrentDispatch) {
        // SAFETY: replacing owner state cannot move CpuLocal. The remote
        // scheduling snapshot is committed under the runqueue lock before a
        // concurrent wake may compare preemption priority.
        let this = unsafe { self.get_unchecked_mut() };
        let snapshot = dispatch.schedule_snapshot();
        let mut run_queue = this.remote.lock_run_queue();
        this.dispatch.current_dispatch = Some(dispatch);
        run_queue.set_current(Some(snapshot));
    }

    pub(crate) fn take_dispatch(self: Pin<&mut Self>) -> Option<CurrentDispatch> {
        // SAFETY: taking owner state cannot move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let mut run_queue = this.remote.lock_run_queue();
        let dispatch = this.dispatch.current_dispatch.take();
        run_queue.set_current(None);
        dispatch
    }

    /// Reads the lock-free lifecycle published by the current dispatch.
    pub(crate) fn current_lifecycle_state(&self) -> Option<ThreadState> {
        self.dispatch
            .current_dispatch
            .as_ref()
            .map(|dispatch| dispatch.runtime_core().state())
    }

    pub(crate) fn charge_current_dispatch(
        self: Pin<&mut Self>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<DispatchCharge, TaskError> {
        // SAFETY: the owner scheduler serializes this pinned runqueue state.
        // These disjoint projections avoid reference-count traffic on every
        // runtime-accounting update.
        let this = unsafe { self.get_unchecked_mut() };
        let remote = &this.remote;
        let mut run_queue = remote.lock_run_queue();
        let bandwidth = run_queue.deadline_bandwidth();
        let admitted_bw_scaled = bandwidth.this_bw_scaled();
        let running_bw_scaled = bandwidth.running_bw_scaled();
        let max_bw_scaled = bandwidth.max_bw_scaled();
        let dispatch_state = &mut this.dispatch;
        let current_is_non_idle =
            dispatch_state.current.is_some() && dispatch_state.current != dispatch_state.idle;
        let grub_reclaimed_ns = dispatch_state
            .current_dispatch
            .as_ref()
            .map_or(0, |dispatch| {
                dispatch.grub_reclaimed_ns(
                    runtime_ns,
                    admitted_bw_scaled.saturating_sub(running_bw_scaled),
                    max_bw_scaled,
                )
            });
        let dispatch = dispatch_state
            .current_dispatch
            .as_mut()
            .ok_or(TaskError::NoRunnableThread)?;
        if current_is_non_idle {
            remote.charge_busy_runtime(runtime_ns);
        }
        let charge = dispatch.charge(
            runtime_ns,
            now_ns,
            reclaimed_ns.saturating_add(grub_reclaimed_ns),
        );
        let current_policy = dispatch.policy;
        let current_fair = dispatch.entity.fair();
        let rt_quota_exempt = dispatch.rt_quota_exempt;
        run_queue.update_fair_virtual_time(current_fair);
        run_queue.set_current(Some(dispatch.schedule_snapshot()));
        let rt_quota_exhausted = if matches!(
            current_policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) {
            dispatch_state.rt_bandwidth.charge(now_ns, runtime_ns)
        } else {
            false
        };
        if charge.slice_expired
            || charge.deadline_overrun
            || (rt_quota_exhausted && !rt_quota_exempt)
        {
            remote.request_reschedule();
        }
        Ok(charge)
    }

    pub(crate) fn settle_current_dispatch(
        mut self: Pin<&mut Self>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<DispatchCharge, TaskError> {
        let runtime_ns = self
            .as_ref()
            .get_ref()
            .dispatch
            .current_dispatch
            .as_ref()
            .ok_or(TaskError::NoRunnableThread)?
            .unaccounted_runtime(now_ns);
        self.as_mut()
            .charge_current_dispatch(now_ns, runtime_ns, reclaimed_ns)
    }

    pub(crate) fn set_idle(self: Pin<&mut Self>, idle: ThreadId, core: Arc<ThreadCore>) {
        debug_assert_eq!(idle, core.id());
        // SAFETY: changing fields does not move this pinned object.
        let fields = unsafe { self.get_unchecked_mut() };
        fields.dispatch.idle = Some(idle);
        fields.dispatch.idle_core = Some(core);
        fields.remote.publish_idle_thread(idle);
        fields.remote.mark_scheduler_ready();
    }

    pub(crate) fn stage_switch_handoff(
        self: Pin<&mut Self>,
        previous: Arc<ThreadCore>,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        let handoff = &mut self.dispatch_state_mut().switch_handoff;
        if handoff.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        *handoff = Some(SwitchHandoff {
            previous,
            migration_target,
            runtime_tail_finished: false,
        });
        Ok(())
    }

    pub(crate) fn finish_switch_runtime_tail(
        self: Pin<&mut Self>,
        previous: ThreadId,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        let handoff = self
            .dispatch_state_mut()
            .switch_handoff
            .as_mut()
            .ok_or(TaskError::InvalidConfiguration)?;
        if handoff.previous.id() != previous
            || handoff.migration_target != migration_target
            || handoff.runtime_tail_finished
        {
            return Err(TaskError::InvalidConfiguration);
        }
        handoff.runtime_tail_finished = true;
        Ok(())
    }

    pub(crate) fn take_switch_handoff(self: Pin<&mut Self>) -> Option<SwitchHandoff> {
        self.dispatch_state_mut().switch_handoff.take()
    }

    pub(crate) fn switch_handoff(&self) -> Option<&SwitchHandoff> {
        self.dispatch.switch_handoff.as_ref()
    }

    pub(crate) fn scheduler_enter(self: Pin<&mut Self>) -> bool {
        // `need_resched` is cleared only after entering the scheduler, never by
        // wake, timer, IPI, or preemption-disable paths. The AcqRel claim pairs
        // with producer Release stores after inbox publication. Rechecking the
        // inbox after the claim closes the race where a forced scheduling path
        // otherwise overwrote a remote producer's doorbell.
        self.remote.scheduler_enter()
    }

    pub(crate) fn defer_park_preemption(&self, requested: bool) {
        self.remote.defer_park_preemption(requested);
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        self.remote.finish_park_preemption(resume_running);
    }

    pub(crate) const fn dispatch_state(&self) -> &dispatch_state::OwnerDispatchState {
        &self.dispatch
    }

    pub(crate) fn dispatch_state_mut(
        self: Pin<&mut Self>,
    ) -> &mut dispatch_state::OwnerDispatchState {
        // SAFETY: the owner borrow is pinned, and OwnerDispatchState contains
        // no self-referential pointer that can move CpuLocal.
        &mut unsafe { self.get_unchecked_mut() }.dispatch
    }

    pub(crate) fn lock_run_queue(&self) -> IrqTicketGuard<'_, CpuRunQueueState> {
        self.remote.lock_run_queue()
    }

    fn task_deadline_state_mut(
        self: Pin<&mut Self>,
    ) -> &mut deadline_state::LocalTaskDeadlineState {
        // SAFETY: the owner borrow is pinned, and moving neither the queue nor
        // its preallocated output storage can move CpuLocal.
        &mut unsafe { self.get_unchecked_mut() }.task_deadlines
    }

    pub(crate) fn drain_state_mut(self: Pin<&mut Self>) -> &mut drain_state::OwnerDrainScratch {
        // SAFETY: scratch buffers are owner-only and do not move CpuLocal.
        &mut unsafe { self.get_unchecked_mut() }.drain
    }

    pub(crate) const fn drain_state(&self) -> &drain_state::OwnerDrainScratch {
        &self.drain
    }

    #[cfg(test)]
    pub(crate) fn deadline_members_are_empty_for_test(&self) -> bool {
        self.remote.lock_run_queue().deadline_members_are_empty()
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        self.remote.balance_request_node()
    }

    #[cfg(test)]
    pub(crate) fn add_deadline_bandwidth(
        self: Pin<&mut Self>,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        self.remote
            .lock_run_queue()
            .add_deadline_bandwidth(utilization_scaled, active)
    }

    /// Returns the owner runqueue's GRUB bandwidth accounting.
    pub fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        self.remote.lock_run_queue().deadline_bandwidth()
    }

    pub(crate) fn scheduler_deadline_due(self: Pin<&mut Self>, now_ns: u64) -> bool {
        // SAFETY: the scheduler owns this pinned runqueue while refreshing RT
        // bandwidth periods and querying its next local event.
        unsafe { self.get_unchecked_mut() }
            .scheduler_deadline_ns(now_ns)
            .is_some_and(|deadline| deadline <= now_ns)
    }

    pub(crate) fn next_oneshot_deadline_ns(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Option<u64> {
        // SAFETY: clockevent selection is an owner-only transition. The
        // mutable queue/scheduler projections cannot move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let deferred_timer_backlog = this.remote.deadline_work_pending()
            && this
                .task_deadlines
                .queue
                .has_immediately_actionable_entry(now_ns);
        let timer = if deferred_timer_backlog {
            // A bounded hard-IRQ pass already published sticky owner work and
            // need_resched. Re-arming the overdue heap head at the hardware
            // resolution would create an interrupt storm that can prevent the
            // scheduler safe point from draining that work. Keep future
            // scheduler deadlines visible and let the runtime's periodic
            // source remain the failsafe clockevent.
            None
        } else {
            this.task_deadlines
                .queue
                .next_deadline_ns(now_ns, timer_resolution_ns)
        };
        let earliest_future_ns = now_ns
            .checked_add(timer_resolution_ns.max(1))
            .or_else(|| now_ns.checked_add(1));
        let scheduler = match this.scheduler_deadline_ns(now_ns) {
            Some(deadline) if deadline <= now_ns => {
                // Linux does not start a scheduler hrtimer whose expiry has
                // already passed: the owning runqueue handles that state
                // immediately. The owner state remains the only deadline
                // authority; sticky work forces a scheduler safe point without
                // manufacturing a resolution-rate interrupt loop.
                this.remote.request_scheduler_work();
                None
            }
            Some(deadline) => earliest_future_ns.map(|earliest| deadline.max(earliest)),
            None => None,
        };
        match (timer, scheduler) {
            (Some(timer), Some(scheduler)) => Some(timer.min(scheduler)),
            (Some(timer), None) => Some(timer),
            (None, Some(scheduler)) => Some(scheduler),
            (None, None) => None,
        }
    }

    pub(crate) fn next_task_deadline_update(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Result<TaskDeadlineUpdate, TaskError> {
        self.prepare_task_deadline_update(now_ns, timer_resolution_ns, true)?
            .ok_or(TaskError::InvalidConfiguration)
    }

    pub(crate) fn next_task_deadline_update_if_changed(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Result<Option<TaskDeadlineUpdate>, TaskError> {
        self.prepare_task_deadline_update(now_ns, timer_resolution_ns, false)
    }

    fn prepare_task_deadline_update(
        mut self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
        force: bool,
    ) -> Result<Option<TaskDeadlineUpdate>, TaskError> {
        let deadline = self
            .as_mut()
            .next_oneshot_deadline_ns(now_ns, timer_resolution_ns)
            .and_then(MonotonicDeadline::from_nanos);
        let deferred_work = self.remote.deadline_work_pending();
        let publication = TaskDeadlinePublicationState {
            deadline,
            deferred_work,
        };
        let task_deadlines = self.task_deadline_state_mut();
        if !force && task_deadlines.publication == Some(publication) {
            return Ok(None);
        }
        task_deadlines.generation = task_deadlines
            .generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let update =
            TaskDeadlineUpdate::try_new(task_deadlines.generation, deadline, deferred_work)
                .ok_or(TaskError::InvalidConfiguration)?;
        task_deadlines.publication = Some(publication);
        Ok(Some(update))
    }

    pub(crate) fn invalidate_task_deadline_publication(self: Pin<&mut Self>) {
        self.task_deadline_state_mut().publication = None;
    }

    pub(crate) fn deadline_work_pending(&self) -> bool {
        self.remote.deadline_work_pending()
    }

    pub(crate) fn task_deadline_expiry_due(&self, now_ns: u64) -> bool {
        self.task_deadlines
            .queue
            .has_immediately_actionable_entry(now_ns)
    }

    #[cfg(test)]
    pub(crate) fn set_task_deadline_generation_for_test(self: Pin<&mut Self>, generation: u64) {
        self.task_deadline_state_mut().generation = generation;
    }

    fn scheduler_deadline_ns(&mut self, now_ns: u64) -> Option<u64> {
        let mut next_deadline_ns = None;
        let run_queue = self.remote.lock_run_queue();
        if let Some(deadline) = run_queue.earliest_deadline_event_ns() {
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }

        let current_is_idle =
            self.dispatch.current.is_some() && self.dispatch.current == self.dispatch.idle;
        if !current_is_idle && let Some(dispatch) = self.dispatch.current_dispatch.as_ref() {
            let fair_slice_required = dispatch.entity.fair().is_none_or(|fair| {
                if fair.mode() == FairMode::Idle {
                    run_queue.has_idle_fair()
                } else {
                    run_queue.has_fair()
                }
            });
            if fair_slice_required && let Some(deadline) = dispatch.next_scheduler_event_ns(now_ns)
            {
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
            if dispatch.is_rt() && !dispatch.rt_quota_exempt {
                let remaining = self.dispatch.rt_bandwidth.remaining_runtime_ns(now_ns);
                let deadline = if remaining == 0 {
                    self.dispatch.rt_bandwidth.next_period_ns(now_ns)
                } else {
                    now_ns.saturating_add(remaining)
                };
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
        }
        if run_queue.has_rt() && self.dispatch.rt_bandwidth.is_throttled(now_ns) {
            let deadline = self.dispatch.rt_bandwidth.next_period_ns(now_ns);
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }
        let current_non_idle =
            self.dispatch.current.is_some() && self.dispatch.current != self.dispatch.idle;
        if run_queue.has_fair()
            && run_queue
                .len()
                .saturating_add(usize::from(current_non_idle))
                > 1
        {
            next_deadline_ns = earliest(next_deadline_ns, self.remote.fair_balance_deadline_ns());
        }
        next_deadline_ns
    }

    /// Attempts to return a coherent remotely observable scheduling snapshot.
    pub fn try_load_summary(&self) -> Option<CpuLoadSummary> {
        self.remote.try_load_summary()
    }

    /// Attempts to return the remotely observable queued runnable count.
    pub fn try_runnable_summary(&self) -> Option<usize> {
        self.remote.try_runnable_summary()
    }

    pub(crate) fn fair_balance_due(&self, now_ns: u64) -> bool {
        self.remote.fair_balance_due(now_ns)
    }

    pub(crate) fn reset_fair_balance(self: Pin<&mut Self>, now_ns: u64, minimum_interval_ns: u64) {
        // SAFETY: this owner-only runqueue update does not move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let interval_ns = minimum_interval_ns.max(1);
        this.dispatch.fair_balance_interval_ns = interval_ns;
        this.remote.defer_fair_balance(now_ns, interval_ns);
    }

    pub(crate) fn backoff_fair_balance(
        self: Pin<&mut Self>,
        now_ns: u64,
        minimum_interval_ns: u64,
        maximum_interval_ns: u64,
    ) {
        // SAFETY: this owner-only runqueue update does not move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let minimum_interval_ns = minimum_interval_ns.max(1);
        let maximum_interval_ns = maximum_interval_ns.max(minimum_interval_ns);
        let current_interval_ns = this
            .dispatch
            .fair_balance_interval_ns
            .clamp(minimum_interval_ns, maximum_interval_ns);
        let next_interval_ns = current_interval_ns
            .saturating_mul(2)
            .min(maximum_interval_ns);
        this.dispatch.fair_balance_interval_ns = next_interval_ns;
        this.remote.defer_fair_balance(now_ns, next_interval_ns);
    }

    /// Returns mutable owner-only access to the preallocated task-deadline heap.
    pub fn task_deadlines(self: Pin<&mut Self>) -> &mut TaskDeadlineQueue {
        // SAFETY: the pinned mutable owner borrow excludes every concurrent
        // timer consumer and does not move CpuLocal or its heap.
        &mut unsafe { self.get_unchecked_mut() }.task_deadlines.queue
    }

    /// Expires one bounded timer batch without allocation or callbacks.
    pub fn expire_task_deadlines(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        // SAFETY: hard-IRQ expiry owns this pinned CPU-local state. These
        // projections are disjoint and no projection is moved.
        let this = unsafe { self.get_unchecked_mut() };
        let batch_limit = this.drain.batch_limit();
        let task_deadlines = &mut this.task_deadlines;
        #[cfg(test)]
        {
            task_deadlines.expire_passes += 1;
        }
        let available = task_deadlines
            .expired_buffer
            .len()
            .saturating_sub(task_deadlines.expired_count);
        let request = TaskDeadlineExpireRequest::new(
            now_ns,
            budget.min(batch_limit).min(available),
            timer_resolution_ns,
        );
        let output = &mut task_deadlines.expired_buffer[task_deadlines.expired_count..];
        let batch = task_deadlines.queue.expire(request, output);
        task_deadlines.expired_count += batch.expired();
        if batch.pending() || batch.expired() != 0 {
            this.remote.publish_deadline_work();
        }
        batch
    }

    #[cfg(test)]
    pub(crate) const fn deadline_expire_passes_for_test(&self) -> usize {
        self.task_deadlines.expire_passes
    }

    pub(crate) fn begin_deadline_work(self: Pin<&mut Self>) -> bool {
        self.remote.begin_deadline_work()
    }

    pub(crate) fn finish_deadline_work(self: Pin<&mut Self>, pending: bool) {
        self.remote.finish_deadline_work(pending);
    }

    /// Copies expired timer events to task-context storage.
    ///
    /// Events that do not fit in `output` remain buffered for the next
    /// task-context drain.
    pub fn take_expired_task_deadlines(
        self: Pin<&mut Self>,
        output: &mut [ExpiredTaskDeadline],
    ) -> usize {
        let task_deadlines = self.task_deadline_state_mut();
        let buffered = task_deadlines.expired_count;
        let count = buffered.min(output.len());
        output[..count].copy_from_slice(&task_deadlines.expired_buffer[..count]);
        let remaining = buffered - count;
        task_deadlines
            .expired_buffer
            .copy_within(count..buffered, 0);
        task_deadlines.expired_buffer[remaining..buffered].fill(ExpiredTaskDeadline::EMPTY);
        task_deadlines.expired_count = remaining;
        count
    }

    pub(crate) fn take_expired_park_deadline(self: Pin<&mut Self>) -> Option<ExpiredTaskDeadline> {
        self.take_expired_task_deadline_matching(|event| {
            event
                .kind()
                .is_some_and(|kind| kind.park_generation().is_some())
        })
    }

    pub(crate) fn take_expired_scheduler_deadline(
        self: Pin<&mut Self>,
    ) -> Option<ExpiredTaskDeadline> {
        self.take_expired_task_deadline_matching(|event| {
            event
                .kind()
                .is_some_and(|kind| kind.park_generation().is_none())
        })
    }

    pub(crate) const fn has_expired_task_deadlines(&self) -> bool {
        self.task_deadlines.expired_count != 0
    }

    pub(crate) fn owns_buffered_expiration(&self, registration: &TaskDeadlineRegistration) -> bool {
        self.task_deadlines.expired_buffer[..self.task_deadlines.expired_count]
            .iter()
            .copied()
            .any(|event| {
                event.thread() == Some(registration.thread())
                    && event.token() == registration.token()
                    && event.deadline_ns() == registration.deadline_ns()
                    && event.kind() == Some(registration.kind())
            })
    }

    fn take_expired_task_deadline_matching(
        self: Pin<&mut Self>,
        matches: impl Fn(ExpiredTaskDeadline) -> bool,
    ) -> Option<ExpiredTaskDeadline> {
        let task_deadlines = self.task_deadline_state_mut();
        let index = task_deadlines.expired_buffer[..task_deadlines.expired_count]
            .iter()
            .rposition(|event| event.thread().is_some() && matches(*event))?;
        task_deadlines.expired_count -= 1;
        let last = task_deadlines.expired_count;
        task_deadlines.expired_buffer.swap(index, last);
        Some(core::mem::replace(
            &mut task_deadlines.expired_buffer[last],
            ExpiredTaskDeadline::EMPTY,
        ))
    }

    /// Returns the owner-control publication endpoint for remote CPUs.
    pub fn owner_control_inbox(&self) -> &SchedulerInbox {
        self.remote.owner_control_inbox()
    }

    /// Reports pending remote work before idle or scheduler exit.
    pub fn has_remote_work(&self) -> bool {
        self.remote.has_remote_work()
    }

    /// Publishes the idle/polling state and performs the final WFI recheck.
    pub fn prepare_idle_wait(&self) -> bool {
        self.remote.prepare_idle_wait()
    }

    /// Clears idle/polling publication after WFI returns.
    pub fn finish_idle_wait(&self) {
        self.remote.finish_idle_wait();
    }

    /// Returns whether this CPU is between idle publication and WFI completion.
    pub fn is_idle_polling(&self) -> bool {
        self.remote.is_idle_polling()
    }
}

pub(super) fn nonzero_deadline(deadline_ns: u64) -> Option<u64> {
    (deadline_ns != 0).then_some(deadline_ns)
}

pub(super) fn earliest(current: Option<u64>, candidate: u64) -> Option<u64> {
    Some(current.map_or(candidate, |current| current.min(candidate)))
}
