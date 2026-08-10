//! Owner-only scheduler deadline and soft-timer facade.

use super::*;

fn earliest<T: Ord>(current: Option<T>, candidate: T) -> Option<T> {
    match current {
        Some(current) => Some(current.min(candidate)),
        None => Some(candidate),
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
        let scheduler_due = matches!(
            this.scheduler_clock_event(monotonic_now),
            Some(SchedulerClockEvent::Due)
        );
        if scheduler_due {
            this.remote.request_reschedule();
        }
        let fair_balance_due = if this.has_periodic_fair_balance_work() {
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

    pub(crate) fn next_oneshot_deadline(
        self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> Option<MonotonicDeadline> {
        // SAFETY: clockevent selection is an owner-only transition. The
        // mutable queue/scheduler projections cannot move CpuLocal.
        let this = unsafe { self.get_unchecked_mut() };
        let timer = {
            let deadlines = this.remote.lock_deadline_base();
            if deadlines.softirq_activated {
                // Linux suppresses `softirq_expires_next` only after the hard
                // hrtimer path has set `softirq_activated` and woken
                // `ktimers/%u`. A merely overdue queue head remains the next
                // physical event so the device's minimum delta creates that
                // ownership transfer instead of silently stopping progress.
                None
            } else {
                deadlines.queue.next_deadline()
            }
        };
        let scheduler = match this.scheduler_clock_event(monotonic_now) {
            // Deadline selection is a pure observation. An already-due hard
            // scheduler timer remains a physical clockevent source and the
            // runtime clamps it to the device minimum delta. Only the firing
            // owner may convert it into sticky scheduler work.
            Some(SchedulerClockEvent::Due) => {
                MonotonicDeadline::from_nanos(monotonic_now.as_nanos())
            }
            Some(SchedulerClockEvent::Future(deadline)) => Some(deadline),
            None => None,
        };
        let fair_balance = this.fair_balance_clockevent_deadline();
        let rt_period = this.rt_bandwidth.deadline_for(this.owner);
        [timer, scheduler, fair_balance, rt_period]
            .into_iter()
            .flatten()
            .min()
    }

    pub(crate) fn next_scheduler_deadline_update(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> Result<SchedulerDeadlineUpdate, TaskError> {
        let publication = self.as_mut().scheduler_deadline_publication(monotonic_now);
        let mut task_deadlines = self.remote.lock_deadline_base();
        if task_deadlines.publication == Some(publication) {
            return SchedulerDeadlineUpdate::try_new(
                task_deadlines.generation,
                publication.deadline,
            )
            .ok_or(TaskError::InvalidConfiguration);
        }
        Self::commit_scheduler_deadline_publication(&mut task_deadlines, publication)
    }

    pub(crate) fn next_scheduler_deadline_update_if_changed(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> Result<Option<SchedulerDeadlineUpdate>, TaskError> {
        let publication = self.as_mut().scheduler_deadline_publication(monotonic_now);
        let mut task_deadlines = self.remote.lock_deadline_base();
        if task_deadlines.publication == Some(publication) {
            return Ok(None);
        }
        Self::commit_scheduler_deadline_publication(&mut task_deadlines, publication).map(Some)
    }

    fn scheduler_deadline_publication(
        mut self: Pin<&mut Self>,
        monotonic_now: MonotonicInstant,
    ) -> SchedulerDeadlinePublicationState {
        let deadline = self.as_mut().next_oneshot_deadline(monotonic_now);
        SchedulerDeadlinePublicationState { deadline }
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
        self.remote.lock_deadline_base().publication = None;
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
            .lock_deadline_base()
            .queue
            .has_immediately_actionable_soft_entry(now)
    }

    pub(crate) fn has_due_scheduler_deadline(&self, now: MonotonicInstant) -> bool {
        self.remote
            .lock_deadline_base()
            .queue
            .has_immediately_actionable_scheduler_entry(now)
    }

    #[cfg(test)]
    pub(crate) fn set_scheduler_deadline_generation_for_test(
        self: Pin<&mut Self>,
        generation: u64,
    ) {
        self.remote.lock_deadline_base().generation = generation;
    }

    fn scheduler_clock_event(
        &self,
        monotonic_now: MonotonicInstant,
    ) -> Option<SchedulerClockEvent> {
        let run_queue = self.remote.lock_run_queue();
        let mut due = false;
        let mut next = None;

        let current_is_idle =
            run_queue.current_thread().is_some() && run_queue.current_thread() == run_queue.idle();
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
                    next = earliest(
                        next,
                        monotonic_now.deadline_after(core::time::Duration::from_nanos(delta_ns)),
                    );
                }
            }
            if dispatch.is_rt()
                && !dispatch.rt_quota_exempt()
                && let Some(remaining) = self.remote.lock_rt_runtime().runtime_until_throttle()
            {
                if remaining == 0 {
                    due = true;
                } else {
                    next = earliest(
                        next,
                        monotonic_now.deadline_after(core::time::Duration::from_nanos(remaining)),
                    );
                }
            }
        }
        if due {
            Some(SchedulerClockEvent::Due)
        } else {
            next.map(SchedulerClockEvent::Future)
        }
    }

    fn fair_balance_clockevent_deadline(&self) -> Option<MonotonicDeadline> {
        if !self.has_periodic_fair_balance_work() {
            return None;
        }
        self.dispatch.fair_balance_deadline()
    }

    fn has_periodic_fair_balance_work(&self) -> bool {
        let run_queue = self.remote.lock_run_queue();
        let current_non_idle =
            run_queue.current_thread().is_some() && run_queue.current_thread() != run_queue.idle();
        run_queue.has_fair() && run_queue.nr_running() > usize::from(current_non_idle)
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
        if !this.has_periodic_fair_balance_work() {
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

    pub(crate) fn lock_deadline_base(
        &self,
    ) -> IrqTicketGuard<'_, crate::system::cpu::remote::CpuDeadlineState> {
        self.remote.lock_deadline_base()
    }

    /// Expires one bounded batch of task-context park timeouts.
    ///
    /// CBS and zero-lag timers are consumed separately in the hard clockevent
    /// path. Only park timeout identities cross into the deferred worker.
    pub(crate) fn on_task_clock_event(
        self: Pin<&mut Self>,
        now: MonotonicInstant,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        let mut this = self;
        let batch = this.as_mut().promote_due_task_deadlines(now, budget);
        if batch.pending() || batch.expired() != 0 {
            this.remote.publish_ktimer_work();
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
        let mut task_deadlines = self.remote.lock_deadline_base();
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
        let mut task_deadlines = self.remote.lock_deadline_base();
        let mut event = [ExpiredTaskDeadline::EMPTY; 1];
        let batch = task_deadlines
            .queue
            .expire_scheduler(TaskDeadlineExpireRequest::new(now, 1), &mut event);
        ((batch.expired() == 1).then_some(event[0]), batch.pending())
    }

    #[cfg(test)]
    pub(crate) fn deadline_expire_passes_for_test(&self) -> usize {
        self.remote.lock_deadline_base().expire_passes
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
        let mut task_deadlines = self.remote.lock_deadline_base();
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
            .lock_deadline_base()
            .take_buffered_expiration(registration)
    }

    pub(crate) fn has_expired_task_deadlines(&self) -> bool {
        self.remote.lock_deadline_base().expired_count != 0
    }

    /// Completes one Linux-style soft hrtimer drain transaction.
    ///
    /// `pending` retains ownership in `ktimers/%u`; otherwise the queue once
    /// again owns its earliest physical clockevent deadline.
    pub(crate) fn finish_task_deadline_softirq(self: Pin<&mut Self>, pending: bool) {
        self.remote.lock_deadline_base().softirq_activated = pending;
    }
}
