//! Owner-only scheduler deadline and soft-timer facade.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SoftTimerExpireBatch {
    expired: usize,
    pending: bool,
}

pub(crate) enum KtimerServiceClaim {
    Kernel(KernelTimerExecution),
    Task(ExpiredTaskDeadline),
    Reap(KernelTimerEntry),
}

pub(crate) enum HardTimerServiceClaim {
    Kernel(KernelTimerExecution),
    Scheduler(ExpiredTaskDeadline),
}

impl SoftTimerExpireBatch {
    pub(crate) const fn expired(self) -> usize {
        self.expired
    }

    pub(crate) const fn pending(self) -> bool {
        self.pending
    }
}

fn earliest<T: Ord>(current: Option<T>, candidate: T) -> Option<T> {
    match current {
        Some(current) => Some(current.min(candidate)),
        None => Some(candidate),
    }
}

/// Deadline inputs derived coherently while one runqueue guard owns current,
/// class membership, runtime accounting, and the periodic balance predicate.
#[derive(Clone, Copy)]
pub(crate) struct SchedulerDeadlineRqObservation {
    clock_event: Option<SchedulerDeadlineRqClockEvent>,
    has_periodic_fair_balance_work: bool,
}

#[derive(Clone, Copy)]
enum SchedulerDeadlineRqClockEvent {
    Due,
    After(core::time::Duration),
}

impl SchedulerDeadlineRqObservation {
    pub(crate) const fn clock_event_due(self) -> bool {
        matches!(self.clock_event, Some(SchedulerDeadlineRqClockEvent::Due))
    }
}

impl SchedulerDeadlineRqClockEvent {
    fn physical_deadline(self, monotonic_now: MonotonicInstant) -> MonotonicDeadline {
        match self {
            // Every already-due value has the same physical meaning. Using the
            // clock origin gives it one stable publication identity while the
            // runtime still clamps it to the device's minimum delta.
            Self::Due => MonotonicDeadline::ORIGIN,
            Self::After(delay) => monotonic_now.deadline_after(delay),
        }
    }
}

enum SchedulerDeadlinePublicationOutcome {
    Unchanged(SchedulerDeadlineUpdate),
    Changed(SchedulerDeadlineUpdate),
}

impl SchedulerDeadlinePublicationOutcome {
    const fn update(self) -> SchedulerDeadlineUpdate {
        match self {
            Self::Unchanged(update) | Self::Changed(update) => update,
        }
    }

    const fn changed_update(self) -> Option<SchedulerDeadlineUpdate> {
        match self {
            Self::Unchanged(_) => None,
            Self::Changed(update) => Some(update),
        }
    }
}

impl CpuLocal {
    /// Returns whether every deadline source already has the publication that
    /// this rq observation would derive without sampling the clock.
    ///
    /// Linux does not restart hrtick or the RT period timer for a plain
    /// FIFO-to-FIFO switch. A class runtime event is relative to rq clock and
    /// therefore still requires a fresh sample. Fair balance and the root RT
    /// period are already absolute deadlines, so the coherent deadline-base
    /// snapshot can prove that they and the task/kernel timer heads are
    /// unchanged. A concurrent timer writer publishes its changed base, while
    /// a newly activated or migrated root RT period retains owner work for a
    /// later derivation.
    pub(crate) fn can_reuse_scheduler_deadline_for_rq_observation(
        &self,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> bool {
        if rq_observation.clock_event.is_some() {
            return false;
        }
        let fair_balance = rq_observation
            .has_periodic_fair_balance_work
            .then(|| self.dispatch.fair_balance_deadline())
            .flatten();
        let rt_period = self.rt_bandwidth.deadline_for(self.owner);
        let non_timer = [fair_balance, rt_period].into_iter().flatten().min();
        self.remote.deadline_publication_snapshot_matches(non_timer)
    }

    pub(crate) fn scheduler_work_due(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> SchedulerDeadlineRqObservation {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .scheduler_work_due_from_rq_observation(monotonic_now, rq_observation)
    }

    pub(crate) fn scheduler_work_due_from_rq_observation(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> SchedulerDeadlineRqObservation {
        // SAFETY: the scheduler owns this pinned runqueue while refreshing RT
        // bandwidth periods and querying its next local event.
        let this = unsafe { self.get_unchecked_mut() };
        if matches!(
            rq_observation.clock_event,
            Some(SchedulerDeadlineRqClockEvent::Due)
        ) {
            this.remote.request_reschedule(RescheduleKind::Immediate);
        }
        if rq_observation.has_periodic_fair_balance_work
            && this.dispatch.publish_fair_balance_due(monotonic_now)
        {
            this.remote.request_scheduler_work();
        }
        rq_observation
    }

    fn next_non_timer_deadline_from_rq_observation(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> Option<MonotonicDeadline> {
        let this = self.as_ref().get_ref();
        // Linux's hrtick callback returns HRTIMER_NORESTART after task_tick().
        // Once that callback has published a sticky preemption request, the
        // fired class-runtime edge has left the physical timer base. The
        // scheduler claim is the next owner and will derive a fresh deadline
        // from the selected rq state before returning with IRQs enabled.
        let scheduler = (!this.remote.immediate_preemption_requested())
            .then(|| {
                rq_observation
                    .clock_event
                    .map(|event| event.physical_deadline(monotonic_now))
            })
            .flatten();
        let fair_balance = rq_observation
            .has_periodic_fair_balance_work
            .then(|| this.dispatch.fair_balance_deadline())
            .flatten();
        let rt_period = this.rt_bandwidth.deadline_for(this.owner);
        [scheduler, fair_balance, rt_period]
            .into_iter()
            .flatten()
            .min()
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .update_scheduler_deadline_publication_if_changed_from_rq_observation(
                monotonic_now,
                rq_observation,
                source,
            )
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed_from_rq_observation(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication_if_changed_from_rq_observation(
                monotonic_now,
                rq_observation,
                source,
            )
    }

    pub(crate) fn next_scheduler_deadline_update_from_rq_observation(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication_from_rq_observation(
                monotonic_now,
                rq_observation,
                source,
            )
            .map(SchedulerDeadlinePublicationOutcome::update)
    }

    fn update_scheduler_deadline_publication_from_rq_observation(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<SchedulerDeadlinePublicationOutcome, TaskError> {
        self.as_ref()
            .get_ref()
            .record_scheduler_deadline_derivation(source);
        // Preserve the established rq/RT-period -> deadline-base lock order.
        // The task/kernel timer head and publication metadata are then read
        // and committed under one authoritative base lock.
        let non_timer = self
            .as_mut()
            .next_non_timer_deadline_from_rq_observation(monotonic_now, rq_observation);
        let mut task_deadlines = self.remote.lock_deadline_publication();
        Self::update_scheduler_deadline_publication_in_base(&mut task_deadlines, non_timer)
    }

    fn update_scheduler_deadline_publication_if_changed_from_rq_observation(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        self.as_ref()
            .get_ref()
            .record_scheduler_deadline_derivation(source);
        let non_timer = self
            .as_mut()
            .next_non_timer_deadline_from_rq_observation(monotonic_now, rq_observation);
        if self.remote.deadline_publication_snapshot_matches(non_timer) {
            return Ok(None);
        }
        let mut task_deadlines = self.remote.lock_deadline_publication();
        Self::update_scheduler_deadline_publication_in_base(&mut task_deadlines, non_timer)
            .map(SchedulerDeadlinePublicationOutcome::changed_update)
    }

    pub(crate) fn prepare_scheduler_deadline_registration_publication(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        source: SchedulerDeadlineDerivationSource,
    ) -> Option<MonotonicDeadline> {
        // Derive rq-owned inputs before entering the timer base. The caller can
        // then mutate the queue and commit its physical publication under one
        // Registration guard, preserving the rq -> deadline-base lock order.
        self.as_ref()
            .get_ref()
            .record_scheduler_deadline_derivation(source);
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.next_non_timer_deadline_from_rq_observation(monotonic_now, rq_observation)
    }

    fn record_scheduler_deadline_derivation(&self, source: SchedulerDeadlineDerivationSource) {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_scheduler_deadline_derivation(source);
        #[cfg(not(feature = "qperf-metrics"))]
        let _ = source;
    }

    fn update_scheduler_deadline_publication_in_base(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        non_timer: Option<MonotonicDeadline>,
    ) -> Result<SchedulerDeadlinePublicationOutcome, TaskError> {
        let timer = task_deadlines.timer_deadline();
        let publication = SchedulerDeadlinePublicationState {
            deadline: [timer, non_timer].into_iter().flatten().min(),
        };
        if task_deadlines.publication == Some(publication) {
            let update =
                SchedulerDeadlineUpdate::try_new(task_deadlines.generation, publication.deadline)
                    .ok_or(TaskError::InvalidConfiguration)?;
            return Ok(SchedulerDeadlinePublicationOutcome::Unchanged(update));
        }
        Self::commit_scheduler_deadline_publication(task_deadlines, publication)
            .map(SchedulerDeadlinePublicationOutcome::Changed)
    }

    pub(crate) fn update_scheduler_deadline_registration_publication(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        non_timer: Option<MonotonicDeadline>,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        // `task_deadlines` already owns the queue mutation. Reusing that guard
        // matches Linux hrtimer enqueue/remove plus expires-next reprogramming.
        Self::update_scheduler_deadline_publication_in_base(task_deadlines, non_timer)
            .map(SchedulerDeadlinePublicationOutcome::update)
    }

    pub(crate) fn update_scheduler_deadline_registration_publication_if_changed(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        non_timer: Option<MonotonicDeadline>,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        Self::update_scheduler_deadline_publication_in_base(task_deadlines, non_timer)
            .map(SchedulerDeadlinePublicationOutcome::changed_update)
    }

    fn commit_scheduler_deadline_publication(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        publication: SchedulerDeadlinePublicationState,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        task_deadlines.generation = task_deadlines
            .generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let update =
            SchedulerDeadlineUpdate::try_new(task_deadlines.generation, publication.deadline)
                .ok_or(TaskError::InvalidConfiguration)?;
        task_deadlines.publication = Some(publication);
        Ok(update)
    }

    fn scheduler_deadline_rq_observation(&self) -> SchedulerDeadlineRqObservation {
        let run_queue = self
            .remote
            .lock_run_queue(RunQueueGuardSource::TimerDeadlineDerivationObservation);
        self.scheduler_deadline_rq_observation_in_run_queue(&run_queue)
    }

    pub(crate) fn scheduler_deadline_rq_observation_in_run_queue(
        &self,
        run_queue: &CpuRunQueueState,
    ) -> SchedulerDeadlineRqObservation {
        let mut due = false;
        let mut next = None;

        let current_thread = run_queue.current_thread();
        let idle = run_queue.idle();
        let current_is_idle = current_thread.is_some() && current_thread == idle;
        if !current_is_idle
            && run_queue.current().is_some()
            && run_queue.current_runtime_timer_required()
            && let Some(delta_ns) = run_queue.current_runtime_timer_delta_ns()
        {
            if delta_ns == 0 {
                due = true;
            } else {
                next = earliest(next, core::time::Duration::from_nanos(delta_ns));
            }
        }
        let clock_event = if due {
            Some(SchedulerDeadlineRqClockEvent::Due)
        } else {
            next.map(SchedulerDeadlineRqClockEvent::After)
        };
        let current_non_idle = current_thread.is_some() && current_thread != idle;
        let has_periodic_fair_balance_work =
            run_queue.has_fair() && run_queue.nr_running() > usize::from(current_non_idle);
        SchedulerDeadlineRqObservation {
            clock_event,
            has_periodic_fair_balance_work,
        }
    }

    /// Returns one coherent remotely observable scheduling snapshot.
    pub fn load_summary(&self) -> CpuLoadSummary {
        self.remote.load_summary()
    }

    /// Returns the remotely observable queued runnable count.
    pub fn queued_summary(&self) -> usize {
        self.remote.queued_summary()
    }

    pub(crate) fn fair_balance_pending(&self) -> bool {
        self.dispatch.fair_balance_pending()
    }

    pub(crate) fn reset_fair_balance(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        minimum_interval_ns: u64,
    ) {
        // SAFETY: this owner-only runqueue update does not move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let interval_ns = minimum_interval_ns.max(1);
        this.dispatch.fair_balance_interval_ns = interval_ns;
        this.dispatch.defer_fair_balance(now, interval_ns);
    }

    pub(crate) fn backoff_fair_balance(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
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
        this.dispatch.defer_fair_balance(now, next_interval_ns);
    }

    pub(crate) fn clear_fair_balance(self: Pin<&mut Self>) {
        self.dispatch_state_mut().clear_fair_balance();
    }

    /// Expires one bounded batch of task-context park timeouts.
    ///
    /// CBS and zero-lag timers are consumed separately in the hard clockevent
    /// path. Only park timeout identities cross into the deferred worker.
    pub(crate) fn on_task_clock_event(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        budget: usize,
    ) -> SoftTimerExpireBatch {
        let this = self;
        let batch_limit = this.drain.batch_limit();
        let Some(mut deadlines) = this
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
        else {
            return SoftTimerExpireBatch {
                expired: 0,
                pending: false,
            };
        };
        let task_batch =
            Self::promote_due_task_deadlines_in_base(&mut deadlines, batch_limit, now, budget);
        let kernel_batch = deadlines
            .kernel_timers
            .expire_due_soft(now, budget.saturating_sub(task_batch.processed()));
        if task_batch.expired() != 0
            || task_batch.pending()
            || kernel_batch.expired() != 0
            || kernel_batch.pending()
        {
            deadlines.softirq_activated = true;
        }
        let batch = SoftTimerExpireBatch {
            expired: task_batch.expired().saturating_add(kernel_batch.expired()),
            pending: task_batch.pending() || kernel_batch.pending(),
        };
        drop(deadlines);
        if batch.pending() || batch.expired() != 0 {
            this.remote.publish_ktimer_work();
        }

        batch
    }

    /// Selects one task-context timer under one Linux hrtimer-style base lock.
    ///
    /// The returned callback identity has already left the base. Its callback
    /// must run without this guard, then a restartable kernel timer completes
    /// through a separate base transaction, matching `__run_hrtimer()`.
    pub(crate) fn claim_ktimer_service_step(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        task_budget: usize,
    ) -> (Option<KtimerServiceClaim>, bool) {
        let batch_limit = self.drain.batch_limit();
        let Some(mut deadlines) = self
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
        else {
            return (None, false);
        };
        if !deadlines.kernel_timers.has_expired() && deadlines.kernel_timers.has_due_soft(now) {
            deadlines.kernel_timers.expire_due_soft(now, 1);
        }
        if deadlines.expired_count == 0
            && deadlines.queue.has_immediately_actionable_soft_entry(now)
        {
            Self::promote_due_task_deadlines_in_base(&mut deadlines, batch_limit, now, task_budget);
        }
        let claim = if let Some(completed) = deadlines.kernel_timers.claim_completed() {
            Some(KtimerServiceClaim::Reap(completed))
        } else {
            let has_kernel = deadlines.kernel_timers.has_expired();
            let has_task = deadlines.expired_count != 0;
            match deadlines.select_service_claim_class(has_kernel, has_task) {
                Some(KtimerClaimClass::Kernel) => {
                    let execution = deadlines
                        .kernel_timers
                        .claim_expired()
                        .expect("an expired kernel timer must remain claimable");
                    Some(KtimerServiceClaim::Kernel(execution))
                }
                Some(KtimerClaimClass::Task) => {
                    let event = deadlines
                        .claim_next_buffered_expiration()
                        .expect("a buffered task expiration must remain claimable");
                    Some(KtimerServiceClaim::Task(event))
                }
                None => None,
            }
        };
        let pending = deadlines.expired_count != 0
            || deadlines.queue.has_immediately_actionable_soft_entry(now)
            || deadlines.kernel_timers.has_expired()
            || deadlines.kernel_timers.has_completed()
            || deadlines.kernel_timers.has_due_soft(now);
        deadlines.softirq_activated = pending;
        (claim, pending)
    }

    fn promote_due_task_deadlines_in_base(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        batch_limit: usize,
        now: MonotonicInstant,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        let expired_count = task_deadlines.expired_count;
        let available = task_deadlines
            .expired_buffer
            .len()
            .saturating_sub(expired_count);
        let request = TaskDeadlineExpireRequest::new(now, budget.min(batch_limit).min(available));
        let crate::system::cpu::remote::CpuDeadlineState {
            queue,
            expired_buffer,
            ..
        } = &mut *task_deadlines;
        let output = &mut expired_buffer[expired_count..];
        let batch = queue.expire_soft(request, output);
        task_deadlines.expired_count += batch.expired();
        batch
    }

    /// Claims one earliest due hard timer from the shared owner hrtimer base.
    pub(crate) fn claim_due_hard_timer(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
    ) -> Option<HardTimerServiceClaim> {
        let mut task_deadlines = self
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::HardExpiry)?;
        let scheduler_deadline = task_deadlines.queue.next_scheduler_deadline();
        let kernel_deadline = task_deadlines.kernel_timers.next_hard_deadline();
        match (scheduler_deadline, kernel_deadline) {
            (Some(scheduler), Some(kernel)) if kernel < scheduler => task_deadlines
                .kernel_timers
                .claim_due_hard(now)
                .map(HardTimerServiceClaim::Kernel),
            (Some(scheduler), _) if now.reached(scheduler) => {
                let mut event = [ExpiredTaskDeadline::EMPTY; 1];
                let batch = task_deadlines
                    .queue
                    .expire_scheduler(TaskDeadlineExpireRequest::new(now, 1), &mut event);
                (batch.expired() == 1).then_some(HardTimerServiceClaim::Scheduler(event[0]))
            }
            (_, Some(kernel)) if now.reached(kernel) => task_deadlines
                .kernel_timers
                .claim_due_hard(now)
                .map(HardTimerServiceClaim::Kernel),
            _ => None,
        }
    }

    pub(crate) fn complete_hard_kernel_timer_execution(
        self: Pin<&mut Self>,
        execution: KernelTimerExecution,
        action: HardKernelTimerAction,
    ) {
        let mut deadlines = self
            .remote
            .lock_deadline_activity(DeadlineBaseGuardSource::HardExpiry);
        if deadlines
            .kernel_timers
            .complete_hard_execution(execution, action)
        {
            // Callback ownership is reclaimed only by `ktimers/%u`; this bit
            // describes deferred destruction, not the hard deadline that just
            // left the active base.
            deadlines.softirq_activated = true;
            drop(deadlines);
            self.remote.publish_ktimer_work();
        }
    }

    /// Removes only the expiration owned by one move-only registration.
    /// Park commit uses this to resolve its own timeout without running an
    /// unrelated soft-timer batch inside the rq transition.
    pub(crate) fn take_buffered_expiration(
        self: Pin<&mut Self>,
        registration: &TaskDeadlineRegistration,
    ) -> Option<ExpiredTaskDeadline> {
        self.remote
            .lock_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
            .take_buffered_expiration(registration)
    }
}
