//! Soft timer under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Selects one PREEMPT_RT `ktimers/%u` task-context callback.
    ///
    /// Hard IRQ has already moved a bounded set of soft expirations into the
    /// per-CPU deadline base. The worker repeats this operation up to its batch
    /// limit, running each callback outside the base lock, before publishing a
    /// remaining-work generation and yielding.
    pub(crate) fn service_ktimer_work(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<KtimerServiceBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }

        let monotonic_now = task_runtime::monotonic_now();
        let budget = cpu.batch_limit();
        let mut kernel_timer = None;
        let mut task_timer = None;
        let mut completed_timer = None;
        let (claim, pending) = cpu
            .as_mut()
            .claim_ktimer_service_step(monotonic_now, budget);
        match claim {
            Some(KtimerServiceClaim::Kernel(execution)) => {
                kernel_timer = Some(execution);
            }
            Some(KtimerServiceClaim::Task(event)) => {
                task_timer = Some(event);
            }
            Some(KtimerServiceClaim::Reap(entry)) => {
                completed_timer = Some(entry);
            }
            None => {}
        }
        // A claimed callback may return a new deadline for the same stable
        // entry. Keep the expired hardware edge authoritative until the
        // callback completes, then publish one combined complete/rearm update.
        // This matches hrtimer restart semantics and avoids a cancel/arm pair
        // for every periodic callback.
        let update = if kernel_timer.is_some() || task_timer.is_some() {
            None
        } else {
            cpu.as_mut().next_scheduler_deadline_update_if_changed(
                SchedulerDeadlineDerivationSource::KtimerService,
            )?
        };

        Ok(KtimerServiceBatch {
            processed: usize::from(
                kernel_timer.is_some() || task_timer.is_some() || completed_timer.is_some(),
            ),
            pending,
            update,
            kernel_timer,
            task_timer,
            completed_timer,
        })
    }

    pub(crate) fn complete_task_timer_execution(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
        handle: Option<&ThreadHandle>,
    ) -> Result<(Option<ThreadWakeHandle>, Option<SchedulerDeadlineUpdate>), TaskError> {
        let mut deadline_base = cpu
            .remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::SoftExpiry);
        let non_timer = deadline_base.non_timer;
        let cancel_requested = deadline_base
            .complete_claimed_task_expiration(event)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5444_0007, event.token().generation() as usize)
            });
        let completed = !cancel_requested
            && handle
                .is_some_and(|handle| handle.core.complete_sleep_timer(event.token().generation()));
        let update = CpuLocal::update_scheduler_deadline_registration_publication_if_changed(
            &mut deadline_base,
            non_timer,
        )?;
        let wake = completed
            .then(|| handle.expect("a completed task timer retains its handle"))
            .filter(|handle| {
                event.kind().is_some_and(|kind| {
                    kind.park_generation() == Some(handle.core.park_generation())
                })
            })
            .map(ThreadHandle::wake_handle);
        Ok((wake, update))
    }

    pub(crate) fn complete_kernel_timer_execution(
        &self,
        cpu: Pin<&mut CpuLocal>,
        execution: KernelTimerExecution,
        action: KernelTimerAction,
    ) -> Result<KernelTimerCompletionBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let mut deadline_base = cpu
            .remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::SoftExpiry);
        let non_timer = deadline_base.non_timer;
        let completed = deadline_base
            .kernel_timers
            .complete_soft_execution(execution, action);
        let update = CpuLocal::update_scheduler_deadline_registration_publication_if_changed(
            &mut deadline_base,
            non_timer,
        )?;
        Ok(KernelTimerCompletionBatch { completed, update })
    }
}
