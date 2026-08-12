use super::*;

pub(super) enum PiParkAttempt {
    Complete,
    Retry,
    Prepared(ThreadHandle, crate::ParkTicket),
}

/// Enters the scheduler-owned PI mutex slow path.
pub fn pi_mutex_lock_slow(
    lock: PiMutexRef<'_>,
    current: &CurrentThreadToken,
    sequence: u64,
) -> Result<PiMutexLockResult, TaskError> {
    runtime_task_system()?.pi_mutex_lock_slow(lock, current.id(), sequence)
}

/// Performs one scheduler park attempt for a PI waiter.
///
/// The caller must recheck ownership, interruption, and timeout after this
/// function returns. This mirrors Linux `rt_mutex_schedule()`: an unrelated
/// wake returns control to the rtmutex state loop instead of being consumed by
/// an inner uninterruptible wait.
pub fn pi_park_current_once(token: &PiWaitToken) -> Result<(), TaskError> {
    if token.can_claim() || token.is_granted() {
        return Ok(());
    }
    let system = runtime_task_system()?;
    let (current, mut ticket) = match prepare_pi_park_attempt(system, token)? {
        PiParkAttempt::Complete | PiParkAttempt::Retry => return Ok(()),
        PiParkAttempt::Prepared(current, ticket) => (current, ticket),
    };
    if token.can_claim() || token.is_granted() {
        cancel_current_park(&current, &mut ticket)?;
        return Ok(());
    }
    commit_current_park(&current, &mut ticket)
}

pub(super) fn prepare_pi_park_attempt(
    system: &TaskSystem,
    token: &PiWaitToken,
) -> Result<PiParkAttempt, TaskError> {
    let _permit = acquire_blocking_permit()?;
    let current = current_thread_handle()?;
    if current.id() != token.thread_id().into() {
        return Err(TaskError::InvalidPiState);
    }
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    system.drain_owner_control(cpu.as_mut())?;
    if token.can_claim() || token.is_granted() {
        return Ok(PiParkAttempt::Complete);
    }
    match system.prepare_park(cpu.as_mut(), &current)? {
        ParkPrepare::Notified => Ok(PiParkAttempt::Retry),
        ParkPrepare::Prepared(ticket) => Ok(PiParkAttempt::Prepared(current, ticket)),
    }
}

/// Cancels a PI wait token after a handoff-before-block race.
pub fn pi_wait_cancel(token: PiWaitToken) -> Result<(), TaskError> {
    runtime_task_system()?.pi_wait_cancel(token)
}

/// Tries to cancel one PI waiter while preserving an ownerless handoff that
/// already selected it.
pub fn pi_wait_try_cancel(token: &PiWaitToken) -> Result<PiWaitCancelOutcome, TaskError> {
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
    token: &PiWaitToken,
    current: &CurrentThreadToken,
) -> Result<PiMutexClaimOutcome, TaskError> {
    if current.id() != token.thread_id().into() {
        return Err(TaskError::InvalidPiState);
    }
    runtime_task_system()?.pi_mutex_claim(token)
}

/// Tests whether the task-local waiter capability has received handoff.
pub fn pi_waiter_is_granted(token: &PiWaitToken) -> bool {
    let waiter = unsafe {
        // SAFETY: the task-system registration retains this task-local wait
        // state until the token is claimed or cancelled.
        token
            .provider_waiter()
            .cast::<crate::PiWaitState>()
            .as_ref()
    };
    waiter.is_granted(token.generation())
}

/// Tests whether the task-local waiter capability is first in its lock queue.
pub fn pi_waiter_is_top(token: &PiWaitToken) -> bool {
    let waiter = unsafe {
        // SAFETY: identical provider capability contract to
        // `pi_waiter_is_granted` above.
        token
            .provider_waiter()
            .cast::<crate::PiWaitState>()
            .as_ref()
    };
    waiter.is_top(token.generation())
}

/// Tests whether the waiter token's initial owner is still executing.
pub fn pi_initial_owner_is_on_cpu(token: &PiWaitToken) -> Result<bool, TaskError> {
    runtime_task_system()?.pi_initial_owner_is_on_cpu(token)
}

/// Drops the scheduler-owned waiter handle transferred by a physical PI mutex.
///
/// # Safety
///
/// `wait_handle` must be the unique initialized inline handle transferred by
/// `PiMutexCore` after every safe lock reference and waiter became unreachable.
pub unsafe fn pi_drop_wait_handle(wait_handle: *mut ()) {
    unsafe { crate::drop_pi_mutex_wait_handle(wait_handle) };
}
