use super::*;

/// Creates a PI donation edge for the calling waiter.
pub fn pi_wait_start(lock: PiLockId, owner: ThreadId) -> Result<PiWaitToken, TaskError> {
    let waiter = current_thread_id()?;
    runtime_task_system()?.pi_wait_start(lock, waiter, owner)
}

/// Blocks the calling waiter unless handoff already granted its token.
pub fn pi_block_current(token: &PiWaitToken) -> Result<(), TaskError> {
    if token.is_granted() {
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
        if token.is_granted() {
            return Ok(());
        }
        let mut ticket = {
            let permit = acquire_blocking_permit()?;
            match prepare_current_park(&permit)? {
                ParkPrepare::Notified => continue,
                ParkPrepare::Prepared(ticket) => ticket,
            }
        };
        if token.is_granted() {
            cancel_current_park(&mut ticket)?;
            return Ok(());
        }
        commit_current_park(&mut ticket)?;
        if token.is_granted() {
            return Ok(());
        }
    }
}

/// Cancels a PI wait token after a handoff-before-block race.
pub fn pi_wait_cancel(token: PiWaitToken) -> Result<(), TaskError> {
    runtime_task_system()?.pi_wait_cancel(token)
}

/// Prepares the scheduler half of a kernel PI mutex ownership transfer.
pub fn prepare_pi_mutex_handoff(
    lock: PiLockId,
    old_owner: ThreadId,
    next_owner: Option<ThreadId>,
) -> Result<PiMutexHandoff<'static>, TaskError> {
    runtime_task_system()?.prepare_pi_mutex_handoff(lock, old_owner, next_owner)
}

/// Publishes a targeted task-context wake after PI metadata handoff.
pub fn pi_wake(wake: &ThreadWakeHandle) -> Result<(), TaskError> {
    match wake.wake() {
        WakeResult::Notified | WakeResult::AlreadyPending | WakeResult::Exited => Ok(()),
        WakeResult::Unavailable => Err(TaskError::NotInitialized),
    }
}
