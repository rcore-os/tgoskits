//! Owner-only scheduler deadline and soft-timer facade.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SoftTimerExpireBatch {
    expired: usize,
    pending: bool,
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

    #[cfg(test)]
    pub(crate) fn next_oneshot_deadline(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> Option<MonotonicDeadline> {
        let rq_observation = self.scheduler_deadline_rq_observation();
        self.as_mut()
            .next_oneshot_deadline_from_rq_observation(monotonic_now, rq_observation)
    }

    #[cfg(test)]
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
            Self::next_timer_deadline(&deadlines)
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
        let scheduler = match rq_observation.clock_event {
            // Deadline selection is a pure observation. An already-due hard
            // scheduler timer remains a physical clockevent source and the
            // runtime clamps it to the device minimum delta. Only the firing
            // owner may convert it into sticky scheduler work.
            Some(SchedulerDeadlineRqClockEvent::Due) => {
                MonotonicDeadline::from_nanos(monotonic_now.as_nanos())
            }
            Some(SchedulerDeadlineRqClockEvent::After(delay)) => {
                Some(monotonic_now.deadline_after(delay))
            }
            None => None,
        };
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

    fn next_timer_deadline(
        deadlines: &crate::system::cpu::remote::CpuDeadlineState,
    ) -> Option<MonotonicDeadline> {
        if deadlines.softirq_activated {
            // Linux suppresses `softirq_expires_next` only after the hard
            // hrtimer path has set `softirq_activated` and woken
            // `ktimers/%u`. A merely overdue queue head remains the next
            // physical event so the device's minimum delta creates that
            // ownership transfer instead of silently stopping progress.
            None
        } else {
            [
                deadlines.queue.next_deadline(),
                deadlines.kernel_timers.next_deadline(),
            ]
            .into_iter()
            .flatten()
            .min()
        }
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
        self.as_mut()
            .update_scheduler_deadline_publication(monotonic_now, source)
            .map(SchedulerDeadlinePublicationOutcome::changed_update)
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed_from_rq_observation(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        self.as_mut()
            .update_scheduler_deadline_publication_from_rq_observation(
                monotonic_now,
                rq_observation,
                source,
            )
            .map(SchedulerDeadlinePublicationOutcome::changed_update)
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
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_scheduler_deadline_derivation(source);
        #[cfg(test)]
        self.remote.record_scheduler_deadline_derivation_for_test();
        #[cfg(not(feature = "qperf-metrics"))]
        let _ = source;
        // Preserve the established rq/RT-period -> deadline-base lock order.
        // The task/kernel timer head and publication metadata are then read
        // and committed under one authoritative base lock.
        let non_timer = self
            .as_mut()
            .next_non_timer_deadline_from_rq_observation(monotonic_now, rq_observation);
        let mut task_deadlines = self.remote.lock_deadline_publication();
        let timer = Self::next_timer_deadline(&task_deadlines);
        let publication = SchedulerDeadlinePublicationState {
            deadline: [timer, non_timer].into_iter().flatten().min(),
        };
        if task_deadlines.publication == Some(publication) {
            let update =
                SchedulerDeadlineUpdate::try_new(task_deadlines.generation, publication.deadline)
                    .ok_or(TaskError::InvalidConfiguration)?;
            return Ok(SchedulerDeadlinePublicationOutcome::Unchanged(update));
        }
        Self::commit_scheduler_deadline_publication(&mut task_deadlines, publication)
            .map(SchedulerDeadlinePublicationOutcome::Changed)
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

    pub(crate) fn invalidate_scheduler_deadline_publication(self: Pin<&mut Self>) {
        self.remote.lock_deadline_publication().publication = None;
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

    pub(crate) fn has_due_task_deadline(&self, now: MonotonicInstant) -> bool {
        self.remote
            .read_active_deadline_base(DeadlineBaseGuardSource::Observation)
            .is_some_and(|deadlines| deadlines.queue.has_immediately_actionable_soft_entry(now))
    }

    pub(crate) fn has_due_kernel_timer(&self, now: MonotonicInstant) -> bool {
        self.remote
            .read_active_deadline_base(DeadlineBaseGuardSource::Observation)
            .is_some_and(|deadlines| deadlines.kernel_timers.has_due(now))
    }

    pub(crate) fn has_expired_kernel_timer(&self) -> bool {
        self.remote
            .read_active_deadline_base(DeadlineBaseGuardSource::SoftExpiry)
            .is_some_and(|deadlines| deadlines.kernel_timers.has_expired())
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

    #[cfg(test)]
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
            let current_entity = run_queue
                .current_scheduling_entity()
                .expect("current dispatch must have one rq-owned scheduling entity");
            let fair_slice_required = current_entity.fair().is_none_or(|fair| {
                if fair.mode() == FairMode::Idle {
                    run_queue.has_idle_fair()
                } else {
                    run_queue.has_fair()
                }
            });
            if fair_slice_required
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

    #[cfg(test)]
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

    #[cfg(test)]
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
        let mut this = self;
        let task_batch = this.as_mut().promote_due_task_deadlines(now, budget);
        let kernel_batch = this
            .as_mut()
            .promote_due_kernel_timers(now, budget.saturating_sub(task_batch.processed()));
        let batch = SoftTimerExpireBatch {
            expired: task_batch.expired().saturating_add(kernel_batch.expired()),
            pending: task_batch.pending() || kernel_batch.pending(),
        };
        if batch.pending() || batch.expired() != 0 {
            this.remote.publish_ktimer_work();
        }
        batch
    }

    pub(crate) fn promote_due_kernel_timers(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        budget: usize,
    ) -> KernelTimerExpireBatch {
        let Some(mut deadlines) = self
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
        else {
            return KernelTimerExpireBatch::empty();
        };
        let batch = deadlines.kernel_timers.expire_due(now, budget);
        if batch.expired() != 0 || batch.pending() {
            deadlines.softirq_activated = true;
        }
        batch
    }

    /// Moves one bounded batch into task-context storage without publishing a
    /// second scheduler request. The soft-timer worker owns publication for
    /// the complete begin/drain/finish transaction.
    pub(crate) fn promote_due_task_deadlines(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        let batch_limit = self.drain.batch_limit();
        let Some(mut task_deadlines) = self
            .remote
            .lock_active_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
        else {
            return TaskDeadlineExpireBatch::empty();
        };
        #[cfg(test)]
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
        if batch.expired() != 0 || batch.pending() {
            task_deadlines.softirq_activated = true;
        }
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

    #[cfg(test)]
    pub(crate) fn deadline_expire_passes_for_test(&self) -> usize {
        self.remote
            .read_deadline_base(DeadlineBaseGuardSource::SoftExpiry)
            .expire_passes
    }

    /// Copies expired timer events to task-context storage.
    ///
    /// Events that do not fit in `output` remain buffered for the next
    /// task-context drain.
    #[cfg(test)]
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

    pub(crate) fn has_expired_task_deadlines(&self) -> bool {
        self.remote
            .read_active_deadline_base(DeadlineBaseGuardSource::SoftExpiry)
            .is_some_and(|deadlines| deadlines.expired_count != 0)
    }

    /// Completes one Linux-style soft hrtimer drain transaction.
    ///
    /// `pending` retains ownership in `ktimers/%u`; otherwise the queue once
    /// again owns its earliest physical clockevent deadline.
    pub(crate) fn finish_task_deadline_softirq(self: Pin<&mut Self>, pending: bool) {
        self.remote
            .lock_deadline_activity(DeadlineBaseGuardSource::SoftExpiry)
            .softirq_activated = pending;
    }
}
