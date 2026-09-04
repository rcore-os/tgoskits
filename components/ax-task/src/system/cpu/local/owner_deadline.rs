//! Owner-only scheduler deadline and soft-timer facade.

use super::*;
use crate::system::cpu::remote::SchedulerNonTimerDeadlines;

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
    Park(Option<Arc<ThreadCore>>),
    Scheduler(ExpiredTaskDeadline),
}

pub(crate) enum HardTimerServiceStep {
    Claim(HardTimerServiceClaim),
    Complete { soft: SoftTimerExpireBatch },
}

impl SoftTimerExpireBatch {
    pub(crate) const fn expired(self) -> usize {
        self.expired
    }

    pub(crate) const fn pending(self) -> bool {
        self.pending
    }
}

/// Deadline inputs derived coherently while one runqueue guard owns current,
/// class membership, runtime accounting, and the periodic balance predicate.
#[derive(Clone, Copy)]
pub(crate) struct SchedulerDeadlineRqObservation {
    runtime_deadline: SchedulerRuntimeDeadline,
    has_periodic_fair_balance_work: bool,
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
    /// Returns whether every shared deadline source already has the
    /// publication that this rq observation would derive.
    ///
    /// Linux does not restart hrtick or the RT period timer for a plain
    /// FIFO-to-FIFO switch. Fair balance and the root RT period are absolute
    /// shared deadlines, so the coherent deadline-base snapshot can prove that
    /// they and the task/kernel timer heads are unchanged. The current class's
    /// hrtick is rq-local and is published separately by the owner CPU.
    pub(crate) fn can_reuse_scheduler_deadline_for_rq_observation(
        &self,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> bool {
        let fair_balance = rq_observation
            .has_periodic_fair_balance_work
            .then(|| self.dispatch.fair_balance_deadline())
            .flatten();
        let rt_period = self.rt_bandwidth.deadline_for(self.owner);
        let non_timer = SchedulerNonTimerDeadlines {
            deadline: [fair_balance, rt_period].into_iter().flatten().min(),
        };
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
            rq_observation.runtime_deadline,
            SchedulerRuntimeDeadline::Due
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

    pub(crate) fn scheduler_runtime_deadline_for_rq_observation(
        &self,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> SchedulerRuntimeDeadline {
        if self.remote.immediate_preemption_requested() {
            SchedulerRuntimeDeadline::Disarmed
        } else {
            rq_observation.runtime_deadline
        }
    }

    pub(crate) fn next_scheduler_runtime_deadline_update(
        mut self: Pin<&mut Self>,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> Option<SchedulerRuntimeDeadline> {
        let deadline = self
            .as_ref()
            .get_ref()
            .scheduler_runtime_deadline_for_rq_observation(rq_observation);
        // SAFETY: the scheduler owner exclusively mutates this pinned local
        // hrtick state while local IRQs remain disabled.
        let this = unsafe { self.as_mut().get_unchecked_mut() };
        if this.scheduler_runtime_deadline == deadline {
            return None;
        }
        this.scheduler_runtime_deadline = deadline;
        Some(deadline)
    }

    fn next_non_timer_deadline_from_rq_observation(
        &self,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> SchedulerNonTimerDeadlines {
        let fair_balance = rq_observation
            .has_periodic_fair_balance_work
            .then(|| self.dispatch.fair_balance_deadline())
            .flatten();
        let rt_period = self.rt_bandwidth.deadline_for(self.owner);
        SchedulerNonTimerDeadlines {
            deadline: [fair_balance, rt_period].into_iter().flatten().min(),
        }
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed(
        mut self: Pin<&mut Self>,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .update_scheduler_deadline_publication_if_changed_from_rq_observation(
                rq_observation,
                source,
            )
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed_from_rq_observation(
        mut self: Pin<&mut Self>,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication_if_changed_from_rq_observation(
                rq_observation,
                source,
            )
    }

    pub(crate) fn next_scheduler_deadline_update_from_rq_observation(
        mut self: Pin<&mut Self>,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication_from_rq_observation(rq_observation, source)
            .map(SchedulerDeadlinePublicationOutcome::update)
    }

    fn update_scheduler_deadline_publication_from_rq_observation(
        self: Pin<&mut Self>,
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
            .as_ref()
            .get_ref()
            .next_non_timer_deadline_from_rq_observation(rq_observation);
        let mut task_deadlines = self.remote.lock_deadline_publication();
        Self::update_scheduler_deadline_publication_in_base(&mut task_deadlines, non_timer)
    }

    fn update_scheduler_deadline_publication_if_changed_from_rq_observation(
        self: Pin<&mut Self>,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        self.as_ref()
            .get_ref()
            .record_scheduler_deadline_derivation(source);
        let non_timer = self
            .as_ref()
            .get_ref()
            .next_non_timer_deadline_from_rq_observation(rq_observation);
        if self.remote.deadline_publication_snapshot_matches(non_timer) {
            return Ok(None);
        }
        let mut task_deadlines = self.remote.lock_deadline_publication();
        Self::update_scheduler_deadline_publication_in_base(&mut task_deadlines, non_timer)
            .map(SchedulerDeadlinePublicationOutcome::changed_update)
    }

    fn record_scheduler_deadline_derivation(&self, source: SchedulerDeadlineDerivationSource) {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_scheduler_deadline_derivation(source);
        #[cfg(not(feature = "qperf-metrics"))]
        let _ = source;
    }

    fn update_scheduler_deadline_publication_in_base(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        non_timer: SchedulerNonTimerDeadlines,
    ) -> Result<SchedulerDeadlinePublicationOutcome, TaskError> {
        task_deadlines.non_timer = non_timer;
        let timer = task_deadlines.timer_deadline();
        let publication = SchedulerDeadlinePublicationState {
            deadline: [timer, non_timer.deadline].into_iter().flatten().min(),
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
        non_timer: SchedulerNonTimerDeadlines,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        // `task_deadlines` already owns the queue mutation. Reusing that guard
        // matches Linux hrtimer enqueue/remove plus expires-next reprogramming.
        Self::update_scheduler_deadline_publication_in_base(task_deadlines, non_timer)
            .map(SchedulerDeadlinePublicationOutcome::update)
    }

    pub(crate) fn update_scheduler_deadline_registration_publication_if_changed(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        non_timer: SchedulerNonTimerDeadlines,
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

    pub(crate) fn scheduler_deadline_rq_observation(&self) -> SchedulerDeadlineRqObservation {
        let run_queue = self
            .remote
            .lock_run_queue(RunQueueGuardSource::TimerDeadlineDerivationObservation);
        self.scheduler_deadline_rq_observation_in_run_queue(&run_queue)
    }

    pub(crate) fn scheduler_deadline_rq_observation_in_run_queue(
        &self,
        run_queue: &CpuRunQueueState,
    ) -> SchedulerDeadlineRqObservation {
        let current_thread = run_queue.current_thread();
        let idle = run_queue.idle();
        let current_is_idle = current_thread.is_some() && current_thread == idle;
        let runtime_deadline = if !current_is_idle
            && run_queue.current().is_some()
            && run_queue.current_runtime_timer_required()
            && let Some(delta_ns) = run_queue.current_runtime_timer_delta_ns()
        {
            if delta_ns == 0 {
                SchedulerRuntimeDeadline::Due
            } else {
                SchedulerRuntimeDeadline::After(core::time::Duration::from_nanos(delta_ns))
            }
        } else {
            SchedulerRuntimeDeadline::Disarmed
        };
        let current_non_idle = current_thread.is_some() && current_thread != idle;
        let has_periodic_fair_balance_work =
            run_queue.has_fair() && run_queue.nr_running() > usize::from(current_non_idle);
        SchedulerDeadlineRqObservation {
            runtime_deadline,
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

    /// Claims one earliest due hard timer or completes this clockevent pass.
    ///
    /// A claimed callback runs after this function releases the base. Once no
    /// hard timer remains due, the same reacquired base transaction promotes
    /// soft expirations and publishes expires-next, matching Linux's
    /// `__hrtimer_run_queues()` plus `hrtimer_update_base()` boundary.
    pub(crate) fn claim_due_hard_timer_step(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        budget: usize,
    ) -> Result<HardTimerServiceStep, TaskError> {
        self.as_ref()
            .get_ref()
            .record_scheduler_deadline_derivation(SchedulerDeadlineDerivationSource::ClockEvent);
        let batch_limit = self.drain.batch_limit();
        let mut task_deadlines = self
            .remote
            .lock_deadline_activity(DeadlineBaseGuardSource::HardExpiry);
        let scheduler_deadline = task_deadlines.queue.next_hard_deadline();
        let kernel_deadline = task_deadlines.kernel_timers.next_hard_deadline();
        let claim = match (scheduler_deadline, kernel_deadline) {
            (Some(scheduler), Some(kernel)) if kernel < scheduler => task_deadlines
                .kernel_timers
                .claim_due_hard(now)
                .map(HardTimerServiceClaim::Kernel),
            (Some(scheduler), _) if now.reached(scheduler) => task_deadlines
                .queue
                .claim_due_hard(now)
                .map(|claim| match claim {
                    HardTaskDeadlineClaim::Park { event, thread } => {
                        let park_generation = event
                            .kind()
                            .and_then(TaskDeadlineKind::park_generation)
                            .expect("a hard park deadline retains its park generation");
                        let completed = thread.complete_sleep_timer(event.token().generation());
                        let ready = completed && thread.park_generation() == park_generation;
                        HardTimerServiceClaim::Park(ready.then_some(thread))
                    }
                    HardTaskDeadlineClaim::Scheduler(event) => {
                        HardTimerServiceClaim::Scheduler(event)
                    }
                }),
            (_, Some(kernel)) if now.reached(kernel) => task_deadlines
                .kernel_timers
                .claim_due_hard(now)
                .map(HardTimerServiceClaim::Kernel),
            _ => None,
        };
        if let Some(claim) = claim {
            return Ok(HardTimerServiceStep::Claim(claim));
        }

        let task_batch =
            Self::promote_due_task_deadlines_in_base(&mut task_deadlines, batch_limit, now, budget);
        let kernel_batch = task_deadlines
            .kernel_timers
            .expire_due_soft(now, budget.saturating_sub(task_batch.processed()));
        if task_batch.expired() != 0
            || task_batch.pending()
            || kernel_batch.expired() != 0
            || kernel_batch.pending()
        {
            task_deadlines.softirq_activated = true;
        }
        let soft = SoftTimerExpireBatch {
            expired: task_batch.expired().saturating_add(kernel_batch.expired()),
            pending: task_batch.pending() || kernel_batch.pending(),
        };
        let non_timer = task_deadlines.non_timer;
        Self::update_scheduler_deadline_publication_in_base(&mut task_deadlines, non_timer)?;
        drop(task_deadlines);
        if soft.pending() || soft.expired() != 0 {
            self.remote.publish_ktimer_work();
        }
        Ok(HardTimerServiceStep::Complete { soft })
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
