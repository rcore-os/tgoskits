use super::*;

pub(super) enum PiParkAttempt {
    Complete,
    Retry,
    Prepared(crate::ParkTicket),
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
    token: &PiWaitToken,
) -> Result<PiParkAttempt, TaskError> {
    let _permit = acquire_blocking_permit()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    if cpu.current() != Some(token.thread_id().into()) {
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

#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
struct AxTaskPiMutexOps;

#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
#[ax_crate_interface::impl_interface]
impl ax_sync::PiMutexTaskOps for AxTaskPiMutexOps {
    fn current_task_id() -> u64 {
        pi_runtime_result(current_thread_id(), "capture current PI mutex task").as_u64()
    }

    fn validate_blocking_context() {
        pi_runtime_result(
            super::validate_blocking_context(),
            "validate PI mutex blocking context",
        );
    }

    fn lock_slow(lock: &ax_sync::PiMutexCore, sequence: u64) -> PiMutexLockResult {
        let current = pi_runtime_result(current_thread_token(), "capture PI mutex waiter");
        let lock = pi_runtime_result(lock.mutex_ref(), "borrow PI mutex identity");
        pi_runtime_result(
            pi_mutex_lock_slow(lock, &current, sequence),
            "register PI mutex waiter",
        )
    }

    fn waiter_is_granted(token: &PiWaitToken) -> bool {
        let waiter = unsafe {
            // SAFETY: the task-system registration retains this task-local
            // wait state until the token is claimed or cancelled.
            token
                .provider_waiter()
                .cast::<crate::PiWaitState>()
                .as_ref()
        };
        waiter.is_granted(token.generation())
    }

    fn waiter_is_top(token: &PiWaitToken) -> bool {
        let waiter = unsafe {
            // SAFETY: identical provider capability contract to
            // `waiter_is_granted` above.
            token
                .provider_waiter()
                .cast::<crate::PiWaitState>()
                .as_ref()
        };
        waiter.is_top(token.generation())
    }

    fn initial_owner_is_on_cpu(token: &PiWaitToken) -> bool {
        pi_runtime_result(
            runtime_task_system().and_then(|system| system.pi_initial_owner_is_on_cpu(token)),
            "observe PI mutex owner execution state",
        )
    }

    fn current_needs_reschedule_pinned() -> bool {
        pi_runtime_result(
            unsafe {
                // SAFETY: the ax-sync owner-spin caller retains its
                // PreemptGuard across this capability call.
                current_needs_reschedule_pinned()
            },
            "observe pinned PI mutex reschedule state",
        )
    }

    fn park_current_once(token: &PiWaitToken) {
        pi_runtime_result(pi_park_current_once(token), "park PI mutex waiter");
    }

    fn try_cancel(token: &PiWaitToken) -> PiWaitCancelOutcome {
        pi_runtime_result(pi_wait_try_cancel(token), "cancel PI mutex waiter")
    }

    fn claim(token: &PiWaitToken) -> PiMutexClaimOutcome {
        let current = pi_runtime_result(current_thread_token(), "capture PI mutex claimant");
        pi_runtime_result(
            pi_mutex_claim(token, &current),
            "claim ownerless PI mutex handoff",
        )
    }

    fn release_owned(lock: &ax_sync::PiMutexCore, old_owner: ax_sync::PiTaskId) {
        let lock = pi_runtime_result(lock.mutex_ref(), "borrow PI mutex release identity");
        pi_runtime_result(
            unsafe {
                // SAFETY: ax-sync produced `old_owner` from this core's
                // owner-authorized release transition.
                pi_mutex_release_owned(lock, old_owner.into())
            },
            "release contended PI mutex",
        );
    }

    fn drop_wait_handle(wait_handle: *mut ()) {
        unsafe {
            // SAFETY: ax-sync transfers the unique installed waiter handle
            // after the physical lock becomes unreachable.
            crate::drop_pi_mutex_wait_handle(wait_handle)
        };
    }
}

#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
#[track_caller]
fn pi_runtime_result<T, E>(result: Result<T, E>, operation: &'static str) -> T
where
    E: core::fmt::Display,
{
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}
