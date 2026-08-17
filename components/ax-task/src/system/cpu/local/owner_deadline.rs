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
    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn exercise_due_scheduler_deadline_republication_for_test(
        mut self: Pin<&mut Self>,
    ) -> Result<crate::task_test_hooks::DeadlinePublicationEntries, TaskError> {
        let owner = self.owner;
        let seed = SchedulerDeadlineRqObservation {
            clock_event: Some(SchedulerDeadlineRqClockEvent::After(
                core::time::Duration::from_nanos(1),
            )),
            has_periodic_fair_balance_work: false,
        };
        let due = SchedulerDeadlineRqObservation {
            clock_event: Some(SchedulerDeadlineRqClockEvent::Due),
            has_periodic_fair_balance_work: false,
        };
        let first_now = MonotonicInstant::from_nanos(100)
            .expect("a task-test monotonic sample must be representable");
        let second_now = MonotonicInstant::from_nanos(200)
            .expect("a task-test monotonic sample must be representable");

        // Seed the real authoritative base with a distinct future scheduler
        // publication, then commit one already-due observation. The second
        // due observation is the transaction under test: it must match the
        // snapshot without entering the base again even though time advanced.
        self.as_mut()
            .next_scheduler_deadline_update_if_changed_from_rq_observation(
                first_now,
                seed,
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?;
        self.as_mut()
            .next_scheduler_deadline_update_if_changed_from_rq_observation(
                first_now,
                due,
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?;
        let owner_index = usize::try_from(owner.as_u32())
            .expect("a runtime CPU identity must fit the local architecture");
        crate::task_test_hooks::arm_deadline_publication_probe(owner_index);
        self.as_mut()
            .next_scheduler_deadline_update_if_changed_from_rq_observation(
                second_now,
                due,
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?;
        let entries = crate::task_test_hooks::take_deadline_publication_entries()
            .ok_or(TaskError::InvalidConfiguration)?;

        // Restore the publication derived from the live runqueue before the
        // IRQ-disabled owner borrow is released. Intermediate test generations
        // were never exposed to the runtime clockevent consumer.
        let monotonic_now = task_runtime::monotonic_now();
        if let Some(update) = self.as_mut().next_scheduler_deadline_update_if_changed(
            monotonic_now,
            SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
        )? {
            task_runtime::publish_scheduler_deadline(update);
        }
        Ok(entries)
    }

    pub(crate) fn scheduler_work_due(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> bool {
        // SAFETY: the scheduler owns this pinned runqueue while refreshing RT
        // bandwidth periods and querying its next local event.
        let this = unsafe { self.get_unchecked_mut() };
        let rq_observation = this.scheduler_deadline_rq_observation();
        let scheduler_due = matches!(
            rq_observation.clock_event,
            Some(SchedulerDeadlineRqClockEvent::Due)
        );
        if scheduler_due {
            this.remote.request_reschedule();
        }
        let fair_balance_due = if rq_observation.has_periodic_fair_balance_work {
            let due = this.dispatch.publish_fair_balance_due(monotonic_now);
            if due {
                this.remote.request_scheduler_work();
            }
            due
        } else {
            false
        };
        scheduler_due || fair_balance_due
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn next_oneshot_deadline(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> Option<MonotonicDeadline> {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .next_oneshot_deadline_from_rq_observation(monotonic_now, rq_observation)
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    fn next_oneshot_deadline_from_rq_observation(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> Option<MonotonicDeadline> {
        let this = self.as_ref().get_ref();
        let timer = if let Some(deadlines) = this
            .remote
            .read_active_deadline_base(DeadlineBaseGuardSource::Observation)
        {
            deadlines.timer_deadline()
        } else {
            None
        };
        let non_timer =
            self.next_non_timer_deadline_from_rq_observation(monotonic_now, rq_observation);
        [timer, non_timer].into_iter().flatten().min()
    }

    fn next_non_timer_deadline_from_rq_observation(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
    ) -> Option<MonotonicDeadline> {
        let this = self.as_ref().get_ref();
        // Deadline selection is a pure observation. An already-due hard
        // scheduler timer remains a physical clockevent source and the
        // runtime clamps it to the device minimum delta. Only the firing
        // owner may convert it into sticky scheduler work.
        let scheduler = rq_observation
            .clock_event
            .map(|event| event.physical_deadline(monotonic_now));
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

    pub(crate) fn next_scheduler_deadline_update(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication(monotonic_now, source)
            .map(SchedulerDeadlinePublicationOutcome::update)
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

    fn update_scheduler_deadline_publication(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<SchedulerDeadlinePublicationOutcome, TaskError> {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .update_scheduler_deadline_publication_from_rq_observation(
                monotonic_now,
                rq_observation,
                source,
            )
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
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::complete_deadline_publication(self.owner);
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
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        self.remote.record_scheduler_deadline_derivation_for_test();
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

    pub(crate) fn publish_hard_timer_work(&self) {
        self.remote.publish_hard_timer_work();
    }

    pub(crate) fn begin_hard_timer_work(self: Pin<&mut Self>) -> bool {
        self.remote.begin_hard_timer_work()
    }

    pub(crate) fn finish_hard_timer_work(self: Pin<&mut Self>, pending: bool) {
        self.remote.finish_hard_timer_work(pending);
    }

    pub(crate) fn has_due_scheduler_deadline(&self, now: MonotonicInstant) -> bool {
        self.remote
            .read_active_deadline_base(DeadlineBaseGuardSource::Observation)
            .is_some_and(|deadlines| {
                deadlines
                    .queue
                    .has_immediately_actionable_scheduler_entry(now)
            })
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn set_scheduler_deadline_generation_for_test(
        self: Pin<&mut Self>,
        generation: u64,
    ) {
        self.remote.lock_deadline_publication().generation = generation;
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
        if !current_is_idle && let Some(dispatch) = run_queue.current() {
            if run_queue.current_runtime_timer_required()
                && let Some(delta_ns) = run_queue.current_runtime_timer_delta_ns()
            {
                if delta_ns == 0 {
                    due = true;
                } else {
                    next = earliest(next, core::time::Duration::from_nanos(delta_ns));
                }
            }
            if dispatch.is_rt()
                && !dispatch.rt_quota_exempt()
                && !run_queue.rt_is_throttled()
                && let Some(remaining) = self.remote.lock_rt_bandwidth().runtime_until_throttle()
            {
                if remaining == 0 {
                    due = true;
                } else {
                    next = earliest(next, core::time::Duration::from_nanos(remaining));
                }
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

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn publish_fair_balance_due(self: Pin<&mut Self>, now: MonotonicInstant) -> bool {
        // SAFETY: this owner-only timer transition does not move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        if !this
            .scheduler_deadline_rq_observation()
            .has_periodic_fair_balance_work
        {
            return false;
        }
        let due = this.dispatch.publish_fair_balance_due(now);
        if due {
            this.remote.request_scheduler_work();
        }
        due
    }

    pub(crate) fn fair_balance_pending(&self) -> bool {
        self.dispatch.fair_balance_pending()
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn fair_balance_deadline_for_test(&self) -> Option<MonotonicDeadline> {
        self.dispatch.fair_balance_deadline()
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
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::complete_deadline_soft_expiry_pass(this.owner);
            return SoftTimerExpireBatch {
                expired: 0,
                pending: false,
            };
        };
        let task_batch =
            Self::promote_due_task_deadlines_in_base(&mut deadlines, batch_limit, now, budget);
        let kernel_batch = deadlines
            .kernel_timers
            .expire_due(now, budget.saturating_sub(task_batch.processed()));
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
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_deadline_soft_expiry_pass(this.owner);
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
        if !deadlines.kernel_timers.has_expired() && deadlines.kernel_timers.has_due(now) {
            deadlines.kernel_timers.expire_due(now, 1);
        }
        if deadlines.expired_count == 0
            && deadlines.queue.has_immediately_actionable_soft_entry(now)
        {
            Self::promote_due_task_deadlines_in_base(&mut deadlines, batch_limit, now, task_budget);
        }
        let has_kernel = deadlines.kernel_timers.has_expired();
        let has_task = deadlines.expired_count != 0;
        let claim_class = deadlines.select_service_claim_class(has_kernel, has_task);
        let claim = match claim_class {
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
        };
        let pending = deadlines.expired_count != 0
            || deadlines.queue.has_immediately_actionable_soft_entry(now)
            || deadlines.kernel_timers.has_expired()
            || deadlines.kernel_timers.has_due(now);
        deadlines.softirq_activated = pending;
        (claim, pending)
    }

    fn promote_due_task_deadlines_in_base(
        task_deadlines: &mut crate::system::cpu::remote::CpuDeadlineState,
        batch_limit: usize,
        now: MonotonicInstant,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        {
            task_deadlines.expire_passes += 1;
        }
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

    /// Removes one due hard scheduler timer from the owner hrtimer base.
    pub(crate) fn take_due_scheduler_deadline(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
    ) -> (Option<ExpiredTaskDeadline>, bool) {
        let Some(mut task_deadlines) = self
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::HardExpiry)
        else {
            return (None, false);
        };
        let mut event = [ExpiredTaskDeadline::EMPTY; 1];
        let batch = task_deadlines
            .queue
            .expire_scheduler(TaskDeadlineExpireRequest::new(now, 1), &mut event);
        ((batch.expired() == 1).then_some(event[0]), batch.pending())
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn deadline_expire_passes_for_test(&self) -> usize {
        self.remote
            .read_deadline_base(DeadlineBaseGuardSource::SoftExpiry)
            .expire_passes
    }

    /// Copies expired timer events to task-context storage.
    ///
    /// Events that do not fit in `output` remain buffered for the next
    /// task-context drain.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn take_expired_task_deadlines(
        self: Pin<&mut Self>,
        output: &mut [ExpiredTaskDeadline],
    ) -> usize {
        let mut task_deadlines = self
            .remote
            .lock_deadline_activity(DeadlineBaseGuardSource::SoftExpiry);
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
