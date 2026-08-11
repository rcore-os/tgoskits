//! ArceOS runtime providers for `ax-sync` capabilities.

pub use ax_task::sync::api::*;

#[cfg(not(feature = "host-test"))]
struct RuntimeCriticalSectionOps;

#[cfg(not(feature = "host-test"))]
#[ax_crate_interface::impl_interface]
impl ax_sync::CriticalSectionOps for RuntimeCriticalSectionOps {
    fn preempt_guard_enter() -> ax_sync::PreemptGuardToken {
        #[cfg(feature = "multitask")]
        {
            return ax_sync::PreemptGuardToken::from_entered(crate::guard::enter_lock_preempt());
        }
        #[cfg(not(feature = "multitask"))]
        ax_sync::PreemptGuardToken::from_entered(false)
    }

    fn preempt_guard_exit(token: ax_sync::PreemptGuardToken) {
        #[cfg(feature = "multitask")]
        if !token.is_none() {
            crate::guard::exit_preempt();
        }
        #[cfg(not(feature = "multitask"))]
        assert!(
            token.is_none(),
            "uniprocessor runtime received a preemption token"
        );
    }

    fn preempt_guard_exit_irq_return(token: ax_sync::PreemptGuardToken) {
        #[cfg(feature = "multitask")]
        if !token.is_none() {
            crate::guard::exit_preempt_from_irq_return();
        }
        #[cfg(not(feature = "multitask"))]
        assert!(
            token.is_none(),
            "uniprocessor runtime received an IRQ-return token"
        );
    }

    fn hardirq_enter() {
        #[cfg(feature = "multitask")]
        crate::irq_time::enter();
    }

    fn hardirq_exit() {
        #[cfg(feature = "multitask")]
        crate::irq_time::exit();
    }

    fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }
}

#[cfg(not(feature = "host-test"))]
struct RuntimePiMutexTaskOps;

#[cfg(not(feature = "host-test"))]
#[ax_crate_interface::impl_interface]
impl ax_sync::PiMutexTaskOps for RuntimePiMutexTaskOps {
    fn current_task_id() -> u64 {
        pi_runtime_result(
            ax_task::sync::bridge::current_thread_id(),
            "capture current PI mutex task",
        )
        .as_u64()
    }

    fn validate_blocking_context() {
        pi_runtime_result(
            ax_task::sync::bridge::validate_blocking_context(),
            "validate PI mutex blocking context",
        );
    }

    fn lock_slow(lock: &ax_sync::PiMutexCore, sequence: u64) -> ax_sync::PiMutexLockResult {
        let current = pi_runtime_result(
            ax_task::sync::bridge::current_thread_token(),
            "capture PI mutex waiter",
        );
        let lock = pi_runtime_result(lock.mutex_ref(), "borrow PI mutex identity");
        pi_runtime_result(
            ax_task::sync::bridge::pi_mutex_lock_slow(lock, &current, sequence),
            "register PI mutex waiter",
        )
    }

    fn waiter_is_granted(token: &ax_sync::PiWaitToken) -> bool {
        ax_task::sync::bridge::pi_waiter_is_granted(token)
    }

    fn waiter_is_top(token: &ax_sync::PiWaitToken) -> bool {
        ax_task::sync::bridge::pi_waiter_is_top(token)
    }

    fn initial_owner_is_on_cpu(token: &ax_sync::PiWaitToken) -> bool {
        pi_runtime_result(
            ax_task::sync::bridge::pi_initial_owner_is_on_cpu(token),
            "observe PI mutex owner execution state",
        )
    }

    fn current_needs_reschedule_pinned() -> bool {
        pi_runtime_result(
            unsafe {
                // SAFETY: the ax-sync owner-spin caller retains its
                // PreemptGuard across this capability call.
                ax_task::sync::bridge::current_needs_reschedule_pinned()
            },
            "observe pinned PI mutex reschedule state",
        )
    }

    fn park_current_once(token: &ax_sync::PiWaitToken) {
        pi_runtime_result(
            ax_task::sync::bridge::pi_park_current_once(token),
            "park PI mutex waiter",
        );
    }

    fn try_cancel(token: &ax_sync::PiWaitToken) -> ax_sync::PiWaitCancelOutcome {
        pi_runtime_result(
            ax_task::sync::bridge::pi_wait_try_cancel(token),
            "cancel PI mutex waiter",
        )
    }

    fn claim(token: &ax_sync::PiWaitToken) -> ax_sync::PiMutexClaimOutcome {
        let current = pi_runtime_result(
            ax_task::sync::bridge::current_thread_token(),
            "capture PI mutex claimant",
        );
        pi_runtime_result(
            ax_task::sync::bridge::pi_mutex_claim(token, &current),
            "claim ownerless PI mutex handoff",
        )
    }

    fn release_owned(lock: &ax_sync::PiMutexCore, old_owner: ax_sync::PiTaskId) {
        let lock = pi_runtime_result(lock.mutex_ref(), "borrow PI mutex release identity");
        pi_runtime_result(
            unsafe {
                // SAFETY: ax-sync produced `old_owner` from this core's
                // owner-authorized release transition.
                ax_task::sync::bridge::pi_mutex_release_owned(lock, old_owner.into())
            },
            "release contended PI mutex",
        );
    }

    fn drop_wait_handle(wait_handle: *mut ()) {
        unsafe {
            // SAFETY: ax-sync transfers the unique installed waiter handle
            // after the physical lock becomes unreachable.
            ax_task::sync::bridge::pi_drop_wait_handle(wait_handle)
        };
    }
}

#[cfg(not(feature = "host-test"))]
#[track_caller]
fn pi_runtime_result<T, E>(result: Result<T, E>, operation: &'static str) -> T
where
    E: core::fmt::Display,
{
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}
