use super::*;

/// Scheduler state that is created explicitly and mutated only by its owner CPU.
///
/// The object is `!Unpin`; runtimes store it in per-CPU pinned allocations and
/// publish it only after registration has completed.
#[derive(Debug)]
pub struct CpuLocal {
    owner: CpuId,
    remote: Arc<CpuRemote>,
    pub(crate) current: Option<ThreadId>,
    pub(crate) current_core: Option<Arc<ThreadCore>>,
    pub(crate) current_dispatch: Option<CurrentDispatch>,
    pub(crate) idle: Option<ThreadId>,
    pub(crate) idle_core: Option<Arc<ThreadCore>>,
    pub(crate) run_queue: RunQueue,
    /// Stable references to Deadline reservations whose GRUB/CBS state is
    /// owned by this CPU, including blocked non-contending reservations that
    /// are absent from both `current` and the runqueue.
    pub(crate) deadline_members: Vec<Arc<ThreadCore>>,
    pub(crate) rt_bandwidth: RtBandwidth,
    deadline_this_bw_scaled: u64,
    deadline_running_bw_scaled: u64,
    deadline_max_bw_scaled: u64,
    pub(crate) task_deadlines: TaskDeadlineQueue,
    pub(crate) remote_wake_buffer: Vec<InboxMessage>,
    pub(crate) migration_buffer: Vec<InboxMessage>,
    deadline_expired_buffer: Vec<ExpiredTaskDeadline>,
    deadline_expired_count: usize,
    task_deadline_generation: u64,
    fair_balance_interval_ns: u64,
    switch_handoff: Option<SwitchHandoff>,
    batch_limit: usize,
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
            current: None,
            current_core: None,
            current_dispatch: None,
            idle: None,
            idle_core: None,
            run_queue: RunQueue::new(),
            deadline_members: Vec::with_capacity(config.timer_capacity()),
            rt_bandwidth: RtBandwidth::new(config.rt_period_ns(), config.rt_runtime_ns()),
            deadline_this_bw_scaled: 0,
            deadline_running_bw_scaled: 0,
            deadline_max_bw_scaled: u64::from(config.deadline_cap_percent()) * 10_000_000,
            task_deadlines: TaskDeadlineQueue::new(config.timer_capacity()),
            remote_wake_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            migration_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            deadline_expired_buffer: vec![ExpiredTaskDeadline::EMPTY; config.batch_limit()],
            deadline_expired_count: 0,
            task_deadline_generation: 0,
            fair_balance_interval_ns: config.balance_interval_ns().max(1),
            switch_handoff: None,
            batch_limit: config.batch_limit(),
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
        self.current
    }

    pub(crate) fn current_core(&self) -> Option<&Arc<ThreadCore>> {
        self.current_core.as_ref()
    }

    /// Clones a strong handle for the currently executing thread.
    ///
    /// This owner-side lookup never consults the generation registry. The
    /// stable core retained by `CpuLocal` pins the registry record and any OS
    /// extension until the returned handle is dropped.
    pub fn current_thread_handle(&self) -> Result<ThreadHandle, TaskError> {
        self.current_core
            .as_ref()
            .map(|core| ThreadHandle::from_core(Arc::clone(core)))
            .ok_or(TaskError::NoRunnableThread)
    }

    /// Returns the configured CPU idle thread, if any.
    pub const fn idle(&self) -> Option<ThreadId> {
        self.idle
    }

    /// Returns the number of runnable non-idle threads.
    pub(crate) const fn runnable_count(&self) -> usize {
        self.run_queue.len()
    }

    pub(crate) fn is_quiescent_for_offline(&self) -> bool {
        (self.current.is_none() || self.current == self.idle)
            && self.run_queue.len() == 0
            && self.deadline_members.is_empty()
            && self.task_deadlines.is_empty()
            && self.deadline_expired_count == 0
            && self.switch_handoff.is_none()
            && self.remote.is_quiescent_for_offline()
    }

    /// Publishes a sticky reschedule request from task or IRQ context.
    pub fn request_reschedule(&self) {
        self.remote.request_reschedule();
    }

    pub(crate) fn request_scheduler_work(&self) {
        self.remote.request_scheduler_work();
    }

    /// Tests the sticky reschedule request without clearing it.
    pub fn needs_reschedule(&self) -> bool {
        self.remote.needs_reschedule()
    }

    /// Returns the preallocated task-deadline capacity selected at construction.
    pub fn timer_capacity(&self) -> usize {
        self.task_deadlines.capacity()
    }

    /// Returns the bounded scheduler safe-point work budget.
    pub const fn batch_limit(&self) -> usize {
        self.batch_limit
    }

    pub(crate) fn clear_current(self: Pin<&mut Self>) {
        let fields = self.fields_mut();
        fields.current = None;
        fields.current_core = None;
        fields.current_dispatch = None;
        fields.remote.publish_current_thread(None);
    }

    pub(crate) fn set_current_core(self: Pin<&mut Self>, core: Arc<ThreadCore>) {
        let id = core.id();
        let fields = self.fields_mut();
        fields.current = Some(id);
        fields.current_core = Some(core);
        fields.remote.publish_current_thread(Some(id));
        fields.remote.mark_scheduler_ready();
    }

    pub(crate) fn install_dispatch(self: Pin<&mut Self>, dispatch: CurrentDispatch) {
        // SAFETY: replacing copy-only owner state cannot move CpuLocal.
        unsafe { self.get_unchecked_mut() }.current_dispatch = Some(dispatch);
    }

    pub(crate) fn take_dispatch(self: Pin<&mut Self>) -> Option<CurrentDispatch> {
        // SAFETY: taking copy-only owner state cannot move CpuLocal.
        unsafe { self.get_unchecked_mut() }.current_dispatch.take()
    }

    /// Reads the lock-free lifecycle published by the current dispatch.
    pub(crate) fn current_lifecycle_state(&self) -> Option<ThreadState> {
        self.current_dispatch
            .as_ref()
            .map(|dispatch| dispatch.runtime_core().state())
    }

    pub(crate) fn charge_current_dispatch(
        self: Pin<&mut Self>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<DispatchCharge, TaskError> {
        let fields = self.fields_mut();
        let current_is_non_idle = fields.current.is_some() && fields.current != fields.idle;
        let grub_reclaimed_ns = fields.current_dispatch.as_ref().map_or(0, |dispatch| {
            dispatch.grub_reclaimed_ns(
                runtime_ns,
                fields
                    .deadline_this_bw_scaled
                    .saturating_sub(fields.deadline_running_bw_scaled),
                fields.deadline_max_bw_scaled,
            )
        });
        let dispatch = fields
            .current_dispatch
            .as_mut()
            .ok_or(TaskError::NoRunnableThread)?;
        if current_is_non_idle {
            fields.remote.charge_busy_runtime(runtime_ns);
        }
        let charge = dispatch.charge(
            runtime_ns,
            now_ns,
            reclaimed_ns.saturating_add(grub_reclaimed_ns),
        );
        let current_policy = dispatch.policy;
        let current_fair = dispatch.entity.fair();
        let rt_quota_exempt = dispatch.rt_quota_exempt;
        fields.run_queue.update_fair_virtual_time(current_fair);
        let rt_quota_exhausted = if matches!(
            current_policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) {
            fields.rt_bandwidth.charge(now_ns, runtime_ns)
        } else {
            false
        };
        if charge.slice_expired
            || charge.deadline_overrun
            || (rt_quota_exhausted && !rt_quota_exempt)
        {
            fields.request_reschedule();
        }
        fields.recompute_scheduler_deadline(now_ns);
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
        fields.idle = Some(idle);
        fields.idle_core = Some(core);
        fields.remote.publish_idle_thread(idle);
        fields.remote.mark_scheduler_ready();
    }

    pub(crate) fn stage_switch_handoff(
        self: Pin<&mut Self>,
        previous: Arc<ThreadCore>,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        let handoff = &mut self.fields_mut().switch_handoff;
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
            .fields_mut()
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
        self.fields_mut().switch_handoff.take()
    }

    pub(crate) fn switch_handoff(&self) -> Option<&SwitchHandoff> {
        self.switch_handoff.as_ref()
    }

    pub(crate) fn register_deadline_member(
        &mut self,
        core: &Arc<ThreadCore>,
    ) -> Result<bool, TaskError> {
        if self
            .deadline_members
            .iter()
            .all(|member| !Arc::ptr_eq(member, core))
        {
            if self.deadline_members.len() == self.deadline_members.capacity() {
                return Err(TaskError::TimerCapacity);
            }
            self.deadline_members.push(Arc::clone(core));
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        if let Some(index) = self
            .deadline_members
            .iter()
            .position(|member| Arc::ptr_eq(member, core))
        {
            self.deadline_members.swap_remove(index);
        }
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

    pub(crate) fn fields_mut(self: Pin<&mut Self>) -> &mut Self {
        // SAFETY: the returned borrow cannot move the `!Unpin` object and is
        // bounded by the pinned mutable borrow.
        unsafe { self.get_unchecked_mut() }
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        self.remote.balance_request_node()
    }

    pub(crate) fn publish_load_summary(
        &self,
        current_key: Option<SchedulingKey>,
        pushable_key: Option<SchedulingKey>,
        runnable_count: usize,
        overloaded: bool,
    ) {
        self.remote
            .publish_load_summary(current_key, pushable_key, runnable_count, overloaded);
    }

    pub(crate) fn add_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let next_this_bw_scaled = self
            .deadline_this_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let next_running_bw_scaled = if active {
            self.deadline_running_bw_scaled
                .checked_add(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_this_bw_scaled = next_this_bw_scaled;
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn remove_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let next_this_bw_scaled = self
            .deadline_this_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let next_running_bw_scaled = if active {
            self.deadline_running_bw_scaled
                .checked_sub(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_this_bw_scaled = next_this_bw_scaled;
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn activate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        let next_running_bw_scaled = self
            .deadline_running_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        if next_running_bw_scaled > self.deadline_this_bw_scaled {
            return Err(TaskError::InvalidConfiguration);
        }
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn deactivate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        self.deadline_running_bw_scaled = self
            .deadline_running_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(())
    }

    /// Returns the owner runqueue's GRUB bandwidth accounting.
    pub const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        DeadlineBandwidthSnapshot {
            this_bw_scaled: self.deadline_this_bw_scaled,
            running_bw_scaled: self.deadline_running_bw_scaled,
            max_bw_scaled: self.deadline_max_bw_scaled,
        }
    }

    pub(crate) fn replace_scheduler_deadline(&self, deadline_ns: Option<u64>) {
        self.remote
            .scheduler_deadline_ns
            .store(deadline_ns.unwrap_or(0), Ordering::Release);
    }

    pub(crate) fn take_due_scheduler_deadline(&self, now_ns: u64) -> bool {
        let mut current = self.remote.scheduler_deadline_ns.load(Ordering::Acquire);
        loop {
            if current == 0 || current > now_ns {
                return false;
            }
            match self.remote.scheduler_deadline_ns.compare_exchange_weak(
                current,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn scheduler_deadline_ns(&self) -> Option<u64> {
        let deadline_ns = self.remote.scheduler_deadline_ns.load(Ordering::Acquire);
        (deadline_ns != 0).then_some(deadline_ns)
    }

    pub(crate) fn refresh_scheduler_deadline(self: Pin<&mut Self>, now_ns: u64) {
        self.fields_mut().recompute_scheduler_deadline(now_ns);
    }

    pub(crate) fn next_oneshot_deadline_ns(
        &self,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Option<u64> {
        let deferred_timer_backlog = self.remote.deadline_work_pending()
            && self.task_deadlines.has_immediately_actionable_entry(now_ns);
        let timer = if deferred_timer_backlog {
            // A bounded hard-IRQ pass already published sticky owner work and
            // need_resched. Re-arming the overdue heap head at the hardware
            // resolution would create an interrupt storm that can prevent the
            // scheduler safe point from draining that work. Keep future
            // scheduler deadlines visible and let the runtime's periodic
            // source remain the failsafe clockevent.
            None
        } else {
            self.task_deadlines
                .next_deadline_ns(now_ns, timer_resolution_ns)
        };
        let earliest_future_ns = now_ns
            .checked_add(timer_resolution_ns.max(1))
            .or_else(|| now_ns.checked_add(1));
        let scheduler = match self.scheduler_deadline_ns() {
            Some(deadline) if deadline <= now_ns => {
                // Linux does not start a scheduler hrtimer whose expiry has
                // already passed: the owning runqueue handles that state
                // immediately. Preserve the same boundary here. Consuming the
                // atomic deadline also clears any matching deferred owner
                // event; sticky work then forces a scheduler safe point
                // without manufacturing a resolution-rate interrupt loop.
                if self.take_due_scheduler_deadline(now_ns) {
                    self.request_scheduler_work();
                }
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
        let deadline = self
            .as_ref()
            .next_oneshot_deadline_ns(now_ns, timer_resolution_ns)
            .and_then(MonotonicDeadline::from_nanos);
        let fields = self.fields_mut();
        fields.task_deadline_generation = fields
            .task_deadline_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        TaskDeadlineUpdate::try_new(
            fields.task_deadline_generation,
            deadline,
            fields.remote.deadline_work_pending(),
        )
        .ok_or(TaskError::InvalidConfiguration)
    }

    #[cfg(test)]
    pub(crate) fn set_task_deadline_generation_for_test(self: Pin<&mut Self>, generation: u64) {
        self.fields_mut().task_deadline_generation = generation;
    }

    fn recompute_scheduler_deadline(&mut self, now_ns: u64) {
        let mut next_deadline_ns = None;
        if let Some(deadline) = self.run_queue.earliest_deadline_event_ns() {
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }

        let current_is_idle = self.current.is_some() && self.current == self.idle;
        if !current_is_idle && let Some(dispatch) = self.current_dispatch.as_ref() {
            let fair_slice_required = dispatch.entity.fair().is_none_or(|fair| {
                if fair.mode() == FairMode::Idle {
                    self.run_queue.has_idle_fair()
                } else {
                    self.run_queue.has_fair()
                }
            });
            if fair_slice_required && let Some(deadline) = dispatch.next_scheduler_event_ns(now_ns)
            {
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
            if dispatch.is_rt() && !dispatch.rt_quota_exempt {
                let remaining = self.rt_bandwidth.remaining_runtime_ns(now_ns);
                let deadline = if remaining == 0 {
                    self.rt_bandwidth.next_period_ns(now_ns)
                } else {
                    now_ns.saturating_add(remaining)
                };
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
        }
        if self.run_queue.has_rt() && self.rt_bandwidth.is_throttled(now_ns) {
            let deadline = self.rt_bandwidth.next_period_ns(now_ns);
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }
        let current_non_idle = self.current.is_some() && self.current != self.idle;
        if self.run_queue.has_fair()
            && self
                .run_queue
                .len()
                .saturating_add(usize::from(current_non_idle))
                > 1
        {
            next_deadline_ns = earliest(
                next_deadline_ns,
                self.remote.fair_balance_deadline_ns.load(Ordering::Acquire),
            );
        }
        self.replace_scheduler_deadline(next_deadline_ns);
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
        let fields = self.fields_mut();
        let interval_ns = minimum_interval_ns.max(1);
        fields.fair_balance_interval_ns = interval_ns;
        fields.remote.defer_fair_balance(now_ns, interval_ns);
    }

    pub(crate) fn backoff_fair_balance(
        self: Pin<&mut Self>,
        now_ns: u64,
        minimum_interval_ns: u64,
        maximum_interval_ns: u64,
    ) {
        let fields = self.fields_mut();
        let minimum_interval_ns = minimum_interval_ns.max(1);
        let maximum_interval_ns = maximum_interval_ns.max(minimum_interval_ns);
        let current_interval_ns = fields
            .fair_balance_interval_ns
            .clamp(minimum_interval_ns, maximum_interval_ns);
        let next_interval_ns = current_interval_ns
            .saturating_mul(2)
            .min(maximum_interval_ns);
        fields.fair_balance_interval_ns = next_interval_ns;
        fields.remote.defer_fair_balance(now_ns, next_interval_ns);
    }

    /// Returns mutable owner-only access to the preallocated task-deadline heap.
    pub fn task_deadlines(self: Pin<&mut Self>) -> &mut TaskDeadlineQueue {
        // SAFETY: the pinned mutable owner borrow excludes every concurrent
        // timer consumer and does not move CpuLocal or its heap.
        &mut unsafe { self.get_unchecked_mut() }.task_deadlines
    }

    /// Expires one bounded timer batch without allocation or callbacks.
    pub fn expire_task_deadlines(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        let fields = self.fields_mut();
        let available = fields
            .deadline_expired_buffer
            .len()
            .saturating_sub(fields.deadline_expired_count);
        let request = TaskDeadlineExpireRequest::new(
            now_ns,
            budget.min(fields.batch_limit).min(available),
            timer_resolution_ns,
        );
        let output = &mut fields.deadline_expired_buffer[fields.deadline_expired_count..];
        let batch = fields.task_deadlines.expire(request, output);
        fields.deadline_expired_count += batch.expired();
        if batch.pending() || batch.expired() != 0 {
            fields.remote.publish_deadline_work();
        }
        batch
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
        let fields = self.fields_mut();
        let buffered = fields.deadline_expired_count;
        let count = buffered.min(output.len());
        output[..count].copy_from_slice(&fields.deadline_expired_buffer[..count]);
        let remaining = buffered - count;
        fields
            .deadline_expired_buffer
            .copy_within(count..buffered, 0);
        fields.deadline_expired_buffer[remaining..buffered].fill(ExpiredTaskDeadline::EMPTY);
        fields.deadline_expired_count = remaining;
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
        self.deadline_expired_count != 0
    }

    fn take_expired_task_deadline_matching(
        self: Pin<&mut Self>,
        matches: impl Fn(ExpiredTaskDeadline) -> bool,
    ) -> Option<ExpiredTaskDeadline> {
        let fields = self.fields_mut();
        let index = fields.deadline_expired_buffer[..fields.deadline_expired_count]
            .iter()
            .rposition(|event| event.thread().is_some() && matches(*event))?;
        fields.deadline_expired_count -= 1;
        let last = fields.deadline_expired_count;
        fields.deadline_expired_buffer.swap(index, last);
        Some(core::mem::replace(
            &mut fields.deadline_expired_buffer[last],
            ExpiredTaskDeadline::EMPTY,
        ))
    }

    /// Returns the migration publication endpoint for remote CPUs.
    pub fn migration_inbox(&self) -> &SchedulerInbox {
        self.remote.migration_inbox()
    }

    /// Returns the deferred-reclaim publication endpoint for remote CPUs.
    pub fn reclaim_inbox(&self) -> &SchedulerInbox {
        self.remote.reclaim_inbox()
    }

    /// Reports pending remote work before idle or scheduler exit.
    pub fn has_remote_work(&self) -> bool {
        self.remote.has_remote_work()
    }

    /// Acknowledges one coalesced scheduler IPI epoch and rechecks publication.
    pub fn acknowledge_scheduler_ipi(&self) {
        self.remote.acknowledge_scheduler_ipi();
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
