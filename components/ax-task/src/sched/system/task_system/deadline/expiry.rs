//! Expiry under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn service_expired_park_deadline(
        &self,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(());
        };
        let handle = match self.thread_handle(thread) {
            Ok(handle) => handle,
            Err(TaskError::StaleThreadId) => return Ok(()),
            Err(error) => return Err(error),
        };
        let completed = handle.core.complete_sleep_timer(event.token().generation());
        let park_matches = event.kind().is_some_and(|kind| {
            kind.park_generation() == Some(handle.core.ordinary_park_generation())
        });
        if completed && park_matches {
            let _wake_result = handle.wake_handle().wake();
        }
        Ok(())
    }

    pub(crate) fn service_expired_scheduler_deadline(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(());
        };
        // Scheduler hard timers must not enter the task-only global registry.
        // The owner rq retains every admitted Deadline core until both timers
        // are cancelled and the reservation is detached, matching Linux's
        // sched_dl_entity-embedded hrtimer lifetime.
        let Some(core) = cpu
            .remote()
            .lock_run_queue(RunQueueGuardSource::DeadlineAccounting)
            .deadline_member(thread)
        else {
            return Ok(());
        };
        match event.kind() {
            Some(TaskDeadlineKind::DeadlineCbs) => {
                self.service_expired_deadline_cbs(cpu, core, event)
            }
            Some(TaskDeadlineKind::DeadlineZeroLag) => {
                self.service_expired_deadline_zero_lag(cpu, core, event)
            }
            Some(TaskDeadlineKind::ParkTimeout { .. }) | None => Ok(()),
        }
    }

    /// Runs every hard timer due at this clock-event sample.
    ///
    /// Linux drains the hard hrtimer queue in `hrtimer_interrupt()` and never
    /// transfers a remainder to task context. Callbacks may mutate or rearm
    /// their stable entries after the deadline-base lock is released; the
    /// complete queue is drained before one combined expires-next update.
    pub(crate) fn service_due_hard_timers(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now: MonotonicInstant,
        budget: usize,
    ) -> Result<HardTimerServiceBatch, TaskError> {
        let mut processed = 0;
        loop {
            let claim = match cpu.as_mut().claim_due_hard_timer_step(now, budget)? {
                HardTimerServiceStep::Claim(claim) => claim,
                HardTimerServiceStep::Complete { soft } => {
                    return Ok(HardTimerServiceBatch { processed, soft });
                }
            };
            match claim {
                HardTimerServiceClaim::Scheduler(event) => {
                    self.service_expired_scheduler_deadline(cpu.as_mut(), event)?;
                }
                HardTimerServiceClaim::Park(thread) => {
                    if let Some(thread) = thread {
                        let _wake_result =
                            self.wake_thread_from_current_cpu(&thread, WakeIntent::Normal);
                    }
                }
                HardTimerServiceClaim::Kernel(mut execution) => {
                    let action = unsafe {
                        // SAFETY: this service runs with the owner CPU's IRQ
                        // baton. The queue lock was released before returning
                        // the explicitly hard-safe callback capability.
                        execution.invoke_hard()
                    };
                    cpu.as_mut()
                        .complete_hard_kernel_timer_execution(execution, action);
                }
            }
            processed += 1;
        }
    }

    pub(super) fn service_expired_deadline_cbs(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !Self::take_expired_registration(&mut sched.deadline.cbs_timer, event) {
            return Ok(());
        }
        if sched.deadline.bandwidth.reservation_owner() != Some(owner) {
            return Err(TaskError::CpuOwnerMismatch {
                expected: sched
                    .deadline
                    .bandwidth
                    .reservation_owner()
                    .map_or(u32::MAX, CpuId::as_u32),
                actual: owner.as_u32(),
            });
        }
        self.refresh_owner_deadline_timers_locked(&core, &mut sched, cpu);
        Ok(())
    }

    pub(super) fn service_expired_deadline_zero_lag(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !Self::take_expired_registration(&mut sched.deadline.zero_lag_timer, event) {
            return Ok(());
        }
        if sched.deadline.bandwidth.reservation_owner() != Some(owner) {
            return Err(TaskError::CpuOwnerMismatch {
                expected: sched
                    .deadline
                    .bandwidth
                    .reservation_owner()
                    .map_or(u32::MAX, CpuId::as_u32),
                actual: owner.as_u32(),
            });
        }
        self.refresh_owner_deadline_timers_locked(&core, &mut sched, cpu);
        Ok(())
    }
}
