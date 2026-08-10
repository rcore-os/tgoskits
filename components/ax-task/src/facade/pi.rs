use super::*;

pub(super) enum PiParkAttempt {
    Complete,
    Retry,
    Prepared(crate::ParkTicket),
}

/// Enters the scheduler-owned PI mutex slow path.
pub fn pi_mutex_lock_slow<'lock>(
    lock: PiMutexRef<'lock>,
    current: &CurrentThreadToken,
    sequence: u64,
) -> Result<PiMutexLockResult<'lock>, TaskError> {
    runtime_task_system()?.pi_mutex_lock_slow(lock, current.id(), sequence)
}

/// Performs one scheduler park attempt for a PI waiter.
///
/// The caller must recheck ownership, interruption, and timeout after this
/// function returns. This mirrors Linux `rt_mutex_schedule()`: an unrelated
/// wake returns control to the rtmutex state loop instead of being consumed by
/// an inner uninterruptible wait.
pub fn pi_park_current_once(token: &PiWaitToken<'_>) -> Result<(), TaskError> {
    if token.can_claim() || token.is_granted() {
        return Ok(());
    }
    let system = runtime_task_system()?;
    let mut ticket = match prepare_pi_park_attempt(system, token)? {
        PiParkAttempt::Complete | PiParkAttempt::Retry => return Ok(()),
        PiParkAttempt::Prepared(ticket) => ticket,
    };
    if token.can_claim() || token.is_granted() {
        cancel_current_park(&mut ticket)?;
        return Ok(());
    }
    commit_current_park(&mut ticket)
}

pub(super) fn prepare_pi_park_attempt(
    system: &TaskSystem,
    token: &PiWaitToken<'_>,
) -> Result<PiParkAttempt, TaskError> {
    let _permit = acquire_blocking_permit()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    if cpu.current() != Some(token.thread_id()) {
        return Err(TaskError::InvalidPiState);
    }
    system.drain_owner_control(cpu.as_mut())?;
    if token.can_claim() || token.is_granted() {
        return Ok(PiParkAttempt::Complete);
    }
    match system.prepare_park(cpu.as_mut())? {
        ParkPrepare::Notified => Ok(PiParkAttempt::Retry),
        ParkPrepare::Prepared(ticket) => Ok(PiParkAttempt::Prepared(ticket)),
    }
}

/// Cancels a PI wait token after a handoff-before-block race.
pub fn pi_wait_cancel(token: PiWaitToken<'_>) -> Result<(), TaskError> {
    runtime_task_system()?.pi_wait_cancel(token)
}

/// Tries to cancel one PI waiter while preserving an ownerless handoff that
/// already selected it.
pub fn pi_wait_try_cancel(token: &PiWaitToken<'_>) -> Result<PiWaitCancelOutcome, TaskError> {
    runtime_task_system()?.pi_wait_try_cancel(token)
}

/// Publishes a raw-mutex-owner PI handoff and wakes the selected waiter.
///
/// # Safety
///
/// `old_owner` must come from [`PiMutexCore::try_release_owned`] on `lock`, and
/// the caller must retain the higher-level raw-mutex owner authority until this
/// complete release transaction returns.
pub unsafe fn pi_mutex_release_owned(
    lock: PiMutexRef<'_>,
    old_owner: ThreadId,
) -> Result<(), TaskError> {
    runtime_task_system()?.pi_mutex_release(lock, old_owner)
}

/// Claims the ownerless PI mutex handoff selected for this waiter.
pub fn pi_mutex_claim(
    token: &PiWaitToken<'_>,
    current: &CurrentThreadToken,
) -> Result<PiMutexClaimOutcome, TaskError> {
    if current.id() != token.thread_id() {
        return Err(TaskError::InvalidPiState);
    }
    runtime_task_system()?.pi_mutex_claim(token)
}
