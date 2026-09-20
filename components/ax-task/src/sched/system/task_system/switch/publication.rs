//! Publication under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Completes every owner-side selection through the same balance and
    /// one-shot programming sequence.
    ///
    /// Forced block and exit paths select a successor just like preemption and
    /// yield. Keeping their tail common prevents a tickless CPU from retaining
    /// the outgoing thread's budget or service deadline after the switch plan
    /// has already committed a different scheduling class.
    pub(in crate::sched::system::task_system) fn finish_owner_selection(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        previous: Option<ThreadId>,
        next: ThreadId,
        previous_urgency: Option<SchedulingUrgency>,
        next_urgency: SchedulingUrgency,
        scheduler_deadline: OwnerSchedulerDeadline,
    ) {
        // Selection, lifecycle, and switch-handoff state are already committed
        // before this tail. Reporting a recoverable error would let block or
        // yield callers attempt to resume an outgoing thread that is no longer
        // current, so runtime failures beyond this boundary are fatal.
        // FIFO has no per-task scheduler deadline. Owner work that races the
        // initial drain already owns a sticky scheduler request, so a plain
        // FIFO-to-FIFO rotation does not scan unrelated idle/Fair/Deadline
        // balance state before returning to the selected task.
        if matches!(scheduler_deadline, OwnerSchedulerDeadline::Unchanged) {
            if previous_urgency != Some(next_urgency) {
                self.notify_overloaded_owners_after_priority_drop(
                    cpu.owner(),
                    previous_urgency,
                    next_urgency,
                );
            }
            return;
        }
        self.notify_overloaded_owners_after_priority_drop(
            cpu.owner(),
            previous_urgency,
            next_urgency,
        );
        let idle = cpu.remote().idle_thread();
        let next_is_idle = idle == Some(next);
        let previous_was_idle = idle.is_some() && previous == idle;
        if next_is_idle && !previous_was_idle {
            // Publish the pull permit before the NOHZ idle target. A racing
            // Fair source may kick this owner as soon as the target bit is
            // visible, including while this scheduler tail is still active.
            cpu.as_mut().arm_idle_pull();
            self.root_domain.publish_fair_idle_target(cpu.owner(), true);
        }
        if previous_was_idle && !next_is_idle {
            self.root_domain
                .publish_fair_idle_target(cpu.owner(), false);
            // Linux `tick_nohz_idle_exit()` runs before `schedule_idle()`
            // leaves the idle task. The idle loop's IRQ-off checkpoints cannot
            // observe a reschedule request that becomes visible only after
            // IRQs are re-enabled, so the committed idle-exit selection owns
            // the periodic tick restart. This precedes every early return so
            // every switch-tail variant observes the same invariant.
            task_runtime::idle_exit_restart_scheduler_tick();
        }
        let rq_baton_retained = cpu
            .as_ref()
            .get_ref()
            .switch_handoff()
            .is_some_and(|handoff| handoff.has_rq_baton());
        let balance_pending = self.owner_balance_work_pending(cpu.as_ref().get_ref(), next);
        let run_queue_changed = if rq_baton_retained && balance_pending {
            // A balance request can race selection publication. Keep it sticky
            // for the first safe point after switch tail; balance paths may
            // open owner rq transactions and therefore cannot run under the
            // inherited raw rq lock.
            cpu.request_scheduler_work();
            false
        } else if balance_pending {
            match self.service_owner_balance(cpu.as_mut(), next) {
                Ok(outcome) => outcome.run_queue_changed(),
                Err(_) => {
                    task_runtime::fatal_invariant(0x5343_0001, next.as_u64() as usize);
                }
            }
        } else {
            false
        };
        // Shared timer heads can remain unchanged while the selected task's
        // hrtick changes. Let the publication layer reuse only the shared
        // deadline and independently commit the selected runtime deadline.
        let timer_result = match (run_queue_changed, scheduler_deadline) {
            (true, _) => self.program_local_timer(
                cpu.as_mut(),
                SchedulerDeadlineDerivationSource::ScheduleSelection,
            ),
            (false, OwnerSchedulerDeadline::Unchanged) => unreachable!(),
            (false, OwnerSchedulerDeadline::Reevaluate(rq_observation)) => self
                .program_local_timer_from_rq_observation(
                    cpu.as_mut(),
                    rq_observation,
                    SchedulerDeadlineDerivationSource::ScheduleSelection,
                ),
        };
        if timer_result.is_err() {
            task_runtime::fatal_invariant(0x5343_0002, next.as_u64() as usize);
        }
    }
}
