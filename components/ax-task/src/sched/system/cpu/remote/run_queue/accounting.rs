//! Accounting under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    /// Accounts the running task in place under the rq lock.
    pub(in crate::sched::system::cpu) fn update_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::RuntimeUpdate,
        )
    }

    /// Accounts a physical non-periodic clockevent.
    ///
    /// When this event consumes a class runtime budget, Linux's hrtick callback
    /// runs the class tick hook and requests ordinary preemption before it
    /// returns from the interrupt. Other timer sources may share the same
    /// physical event; they do not invoke the hook unless accounting proves the
    /// current request expired.
    pub(in crate::sched::system::cpu) fn clock_event_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::ClockEvent,
        )
    }

    /// Applies one Linux scheduler tick after accounting the running task.
    ///
    /// RT and Deadline entities remain linked in their class structure, while
    /// Fair/stop entities remain in `CurrentDispatch`. A clock tick therefore
    /// never has to take the dispatch out of the rq and reinstall it merely to
    /// update runtime, matching Linux `update_curr_*()` ownership.
    pub(in crate::sched::system::cpu) fn task_tick_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
        tick_ns: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::SchedulerTick { tick_ns },
        )
    }

    /// Accounts a periodic scheduler tick coalesced with a scheduler deadline.
    ///
    /// Linux runs both logical callbacks when the periodic tick and hrtick
    /// share one physical interrupt. The periodic hook still performs its
    /// ordinary class maintenance, while an expired Fair request retains the
    /// hrtick callback's immediate preemption semantics.
    pub(in crate::sched::system::cpu) fn task_tick_and_clock_event_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
        tick_ns: u64,
    ) -> Result<RqCurrentUpdate, TaskError> {
        self.update_current_for_event(
            runtime_ns,
            reclaimed_ns,
            deadline_extra_bw_scaled,
            CurrentAccountingEvent::SchedulerTickWithClockEvent { tick_ns },
        )
    }

    pub(super) fn update_current_for_event(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        deadline_extra_bw_scaled: u64,
        event: CurrentAccountingEvent,
    ) -> Result<RqCurrentUpdate, TaskError> {
        let now_ns = self
            .clock
            .snapshot()
            .ok_or(TaskError::InvalidConfiguration)?
            .task()
            .as_nanos();
        let current_thread = self.current_thread().ok_or(TaskError::NoRunnableThread)?;
        if self.idle() == Some(current_thread) {
            self.queue
                .current_mut()
                .expect("current identity must retain its dispatch")
                .account_dedicated_idle_until(now_ns);
            return Ok(RqCurrentUpdate::DedicatedIdle);
        }

        let bandwidth = self.queue.deadline_bandwidth();
        let (mut charge, policy, current_entity, rt_quota_exempt) = self.queue.charge_current(
            runtime_ns,
            now_ns,
            bandwidth.inactive_bw_scaled(),
            deadline_extra_bw_scaled,
            bandwidth.max_bw_scaled(),
            reclaimed_ns,
        )?;
        let deadline_replenish_reschedule = if charge.deadline_replenished {
            self.queue
                .requeue_replenished_deadline_current(current_thread)?
        } else {
            false
        };
        if let Some(current_fair) = current_entity.fair() {
            self.queue.update_fair_virtual_time(Some(current_fair));
        }
        let class_tick = event.runs_class_tick(charge.slice_expired).then(|| {
            SchedulerClass::for_policy(policy).task_tick(
                &mut self.queue,
                current_thread,
                policy,
                &current_entity,
                charge,
                event.periodic_tick_ns(),
            )
        });
        if class_tick.is_some_and(|tick| tick.slice_expired) {
            charge.slice_expired = true;
        }
        let class_tick_reschedule = class_tick.is_some_and(|tick| tick.request_reschedule);
        let deadline_runtime_reschedule =
            matches!(policy, SchedulePolicy::Deadline(_)) && charge.slice_expired;
        let reschedule = if deadline_runtime_reschedule || deadline_replenish_reschedule {
            Some(RescheduleKind::Immediate)
        } else {
            class_tick_reschedule
                .then_some(event.class_reschedule_kind(policy, charge.slice_expired))
        };
        Ok(RqCurrentUpdate::Task {
            charge,
            reschedule,
            realtime: matches!(
                policy,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            ),
            rt_quota_exempt,
        })
    }
}
