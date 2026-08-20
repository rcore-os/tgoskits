//! Runtime-backed task-context kernel timer registration.

use super::*;
use crate::{
    DeadlineBaseGuardSource, SchedulerDeadlineDerivationSource, runtime::SchedulerDeadlineUpdate,
};

enum KernelTimerRegistrationResult {
    Registered(KernelTimerHandle, Option<SchedulerDeadlineUpdate>),
    Rejected(TaskError, KernelTimerEntry),
}

struct KernelTimerCancellationResult {
    outcome: Result<KernelTimerCancelOutcome, TaskError>,
    removed: Option<KernelTimerEntry>,
}

/// Registers a task-context callback on the calling CPU's monotonic clock base.
///
/// Callback ownership is allocated before IRQs are excluded. Hard IRQ only
/// promotes the entry into the existing `ktimers/%u` service; the callback is
/// invoked later without the deadline lock or an IRQ guard held.
///
/// # Errors
///
/// Returns [`TaskError::UnsafeContext`] outside ordinary task context,
/// [`TaskError::TimerCapacity`] when the per-CPU callback base is full, or a
/// runtime/clockevent error without leaving a hidden registration behind.
pub fn register_kernel_timer(
    deadline: MonotonicDeadline,
    callback: KernelTimerCallback,
) -> Result<KernelTimerHandle, TaskError> {
    validate_task_context()?;
    let entry = KernelTimerEntry::new(deadline, callback).map_err(kernel_timer_error)?;
    register_kernel_timer_entry(entry)
}

/// Registers a stable callback that may rearm the same timer identity.
///
/// The callback runs in the owner CPU's `ktimers/%u` task and returns either
/// [`KernelTimerAction::Complete`] or an absolute deadline for the same entry.
/// Cancellation remains non-blocking; if it races with an executing callback,
/// the callback may finish but cannot rearm the cancelled registration.
pub fn register_restartable_kernel_timer(
    deadline: MonotonicDeadline,
    callback: RestartableKernelTimerCallback,
) -> Result<KernelTimerHandle, TaskError> {
    validate_task_context()?;
    let entry =
        KernelTimerEntry::new_restartable(deadline, callback).map_err(kernel_timer_error)?;
    register_kernel_timer_entry(entry)
}

fn register_kernel_timer_entry(entry: KernelTimerEntry) -> Result<KernelTimerHandle, TaskError> {
    let result = {
        let mut irq = RuntimeIrqGuard::enter();
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let owner = cpu.owner();
        let monotonic_now = task_runtime::monotonic_now();
        let non_timer = cpu
            .as_mut()
            .prepare_scheduler_deadline_registration_publication(
                monotonic_now,
                SchedulerDeadlineDerivationSource::KernelTimer,
            );
        let mut deadline_base = cpu
            .remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::Registration);
        let inserted = deadline_base.kernel_timers.insert(owner, entry);
        match inserted {
            Ok(handle) => {
                match CpuLocal::update_scheduler_deadline_registration_publication_if_changed(
                    &mut deadline_base,
                    non_timer,
                ) {
                    Ok(update) => KernelTimerRegistrationResult::Registered(handle, update),
                    Err(error) => {
                        let removed = deadline_base
                            .kernel_timers
                            .cancel(handle)
                            .expect("failed timer publication must roll back its new entry");
                        KernelTimerRegistrationResult::Rejected(error, removed)
                    }
                }
            }
            Err(entry) => KernelTimerRegistrationResult::Rejected(TaskError::TimerCapacity, entry),
        }
    };
    finish_kernel_timer_registration(result)
}

/// Cancels one queued callback without waiting for a callback already claimed.
///
/// A remote cancellation mutates only the original owner base. It may leave a
/// conservative stale hardware edge; only the owner CPU may reprogram its
/// physical comparator.
pub fn cancel_kernel_timer(
    handle: KernelTimerHandle,
) -> Result<KernelTimerCancelOutcome, TaskError> {
    validate_task_context()?;
    let system = runtime_task_system()?;
    let result = {
        let mut irq = RuntimeIrqGuard::enter();
        let mut current = runtime_current_cpu_mut(&mut irq)?;
        let remote = system
            .cpu_remote(handle.owner())
            .ok_or(TaskError::InvalidConfiguration)?;
        let local_owner = current.owner() == handle.owner();
        let non_timer = local_owner.then(|| {
            current
                .as_mut()
                .prepare_scheduler_deadline_registration_publication(
                    task_runtime::monotonic_now(),
                    SchedulerDeadlineDerivationSource::KernelTimer,
                )
        });
        let mut deadline_base =
            remote.lock_deadline_activity(DeadlineBaseGuardSource::Registration);
        let mut removed = deadline_base.kernel_timers.cancel(handle);
        let outcome = if removed.is_some() {
            if let Some(non_timer) = non_timer {
                match CpuLocal::update_scheduler_deadline_registration_publication_if_changed(
                    &mut deadline_base,
                    non_timer,
                ) {
                    Ok(Some(update)) => {
                        drop(deadline_base);
                        task_runtime::publish_scheduler_deadline(update);
                        Ok(KernelTimerCancelOutcome::Cancelled)
                    }
                    Ok(None) => Ok(KernelTimerCancelOutcome::Cancelled),
                    Err(error) => {
                        deadline_base.kernel_timers.restore_cancelled(
                            removed
                                .take()
                                .expect("failed cancellation publication must restore its entry"),
                        );
                        Err(error)
                    }
                }
            } else {
                Ok(KernelTimerCancelOutcome::Cancelled)
            }
        } else {
            Ok(KernelTimerCancelOutcome::NotCancelled)
        };
        KernelTimerCancellationResult { outcome, removed }
    };
    drop(result.removed);
    result.outcome
}

fn finish_kernel_timer_registration(
    result: KernelTimerRegistrationResult,
) -> Result<KernelTimerHandle, TaskError> {
    match result {
        KernelTimerRegistrationResult::Registered(handle, update) => {
            if let Some(update) = update {
                task_runtime::publish_scheduler_deadline(update);
            }
            Ok(handle)
        }
        KernelTimerRegistrationResult::Rejected(error, entry) => {
            drop(entry);
            Err(error)
        }
    }
}

fn kernel_timer_error(error: TaskDeadlineError) -> TaskError {
    match error {
        TaskDeadlineError::Capacity => TaskError::TimerCapacity,
        TaskDeadlineError::GenerationExhausted | TaskDeadlineError::KindMismatch => {
            TaskError::InvalidConfiguration
        }
    }
}
