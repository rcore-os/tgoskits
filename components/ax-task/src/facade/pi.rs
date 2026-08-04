use super::*;

/// Prepares a PI donation edge for publication after local waiter insertion.
pub fn prepare_pi_wait_start(
    lock: PiLockId,
    owner: ThreadId,
) -> Result<PiWaitStart<'static>, TaskError> {
    let waiter = current_thread_id()?;
    runtime_task_system()?.prepare_pi_wait_start(lock, waiter, owner)
}

/// Prepares a waiter in an ownerless mutex claim window.
pub fn prepare_pi_wait_start_pending(
    lock: PiLockId,
    pending_head: ThreadId,
) -> Result<PiWaitStart<'static>, TaskError> {
    let waiter = current_thread_id()?;
    runtime_task_system()?.prepare_pi_wait_start_pending(lock, waiter, pending_head)
}

/// Blocks the calling waiter until it is selected to claim or granted.
pub fn pi_block_current(token: &PiWaitToken) -> Result<(), TaskError> {
    if token.is_selected() || token.is_granted() {
        return Ok(());
    }
    let system = runtime_task_system()?;
    if runtime_current_cpu()?.current() != Some(token.waiter()) {
        return Err(TaskError::InvalidPiState);
    }
    loop {
        {
            let mut irq = RuntimeIrqGuard::enter();
            let now_ns = task_runtime::monotonic_ns();
            let mut cpu = runtime_current_cpu_mut(&mut irq)?;
            system.drain_policy_updates(cpu.as_mut(), now_ns)?;
        }
        if token.is_selected() || token.is_granted() {
            return Ok(());
        }
        let mut ticket = {
            let permit = acquire_blocking_permit()?;
            match prepare_current_park(&permit)? {
                ParkPrepare::Notified => continue,
                ParkPrepare::Prepared(ticket) => ticket,
            }
        };
        if token.is_selected() || token.is_granted() {
            cancel_current_park(&mut ticket)?;
            return Ok(());
        }
        commit_current_park(&mut ticket)?;
        if token.is_selected() || token.is_granted() {
            return Ok(());
        }
    }
}

/// Cancels a PI wait token after a handoff-before-block race.
pub fn pi_wait_cancel(token: PiWaitToken) -> Result<(), TaskError> {
    runtime_task_system()?.pi_wait_cancel(token)
}

/// Prepares the scheduler half of a contended PI mutex release.
pub fn prepare_pi_mutex_release(
    lock: PiLockId,
    old_owner: ThreadId,
    selected: ThreadId,
) -> Result<PiMutexRelease<'static>, TaskError> {
    runtime_task_system()?.prepare_pi_mutex_release(lock, old_owner, selected)
}

/// Prepares the scheduler half of claiming an ownerless PI mutex.
pub fn prepare_pi_mutex_claim(
    lock: PiLockId,
    pending_head: ThreadId,
    claimant: ThreadId,
) -> Result<PiMutexClaim<'static>, TaskError> {
    runtime_task_system()?.prepare_pi_mutex_claim(lock, pending_head, claimant)
}

/// Publishes a targeted task-context wake after PI metadata handoff.
pub fn pi_wake(wake: &ThreadWakeHandle) -> Result<(), TaskError> {
    match wake.wake_from_task() {
        WakeResult::Notified | WakeResult::AlreadyPending | WakeResult::Exited => Ok(()),
        WakeResult::Unavailable => Err(TaskError::NotInitialized),
    }
}
