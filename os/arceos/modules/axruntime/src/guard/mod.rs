//! CPU-local implementation of the lock context runtime.

mod state;
use state::{RuntimeGuardState, RuntimeIrqState, RuntimePreemptState, SchedulerBatonState};

/// Installs the one process-wide user-return boundary probe.
///
/// The hook runs after the final no-work snapshot and must not allocate,
/// block, or enter the scheduler. It is never replaced or removed.

#[ax_percpu::def_percpu]
static RUNTIME_GUARD_STATE: RuntimeGuardState = RuntimeGuardState::new();

pub(crate) fn assert_boot_preemption_held() {
    let state = read_state();
    assert_eq!(
        state.irq,
        RuntimeIrqState::new(),
        "IRQ guard crossed a runtime boot phase"
    );
    assert_eq!(
        state.preempt,
        RuntimePreemptState::new(),
        "preemption guard crossed a runtime boot phase"
    );
    assert_eq!(
        current_preempt_depth(),
        1,
        "boot current must retain PREEMPT_DISABLED until scheduler publication"
    );
}
pub(crate) fn release_bootstrap_preemption() {
    let state = read_state();
    assert!(state.irq.is_clear() && state.preempt.is_clear());
    assert_eq!(
        current_preempt_depth(),
        1,
        "bootstrap release requires the exact Linux boot preemption depth"
    );
    with_current_cpu_pin(cpu_local::release_bootstrap_preemption)
        .unwrap_or_else(|error| panic!("bootstrap preemption owner is invalid: {error}"));
    assert_eq!(
        current_preempt_depth(),
        0,
        "bootstrap preemption depth must be released exactly once"
    );
    // Linux's schedule_preempt_disabled() lowers PREEMPT_DISABLED without
    // clearing need-resched, then enters schedule() unconditionally. Do the
    // same after every scheduler dependency and the task identity are live;
    // the scheduler decision, not bootstrap release, consumes pending work.
    assert!(
        ax_hal::asm::irqs_enabled(),
        "multitask bootstrap must publish IRQ delivery before releasing PREEMPT_DISABLED"
    );
    ax_task::schedule_current_cpu()
        .unwrap_or_else(|error| panic!("bootstrap scheduler entry failed: {error}"));
}

/// Services scheduler work and retains IRQ exclusion through userspace entry.
///
/// On success, the caller must enter `ax_hal::cpu::uspace::UserContext::run`
/// immediately. Its architecture return instruction restores the saved user
/// IRQ state, matching Linux's final IRQ-off `exit_to_user_mode_loop()` check.
pub(crate) fn prepare_user_return() -> Result<(), ax_task::TaskError> {
    if !ax_hal::asm::irqs_enabled() || in_hard_irq() {
        return Err(ax_task::TaskError::UnsafeContext);
    }

    loop {
        ax_hal::asm::disable_irqs();
        // SAFETY: raw IRQ exclusion pins this complete CPU-remote observation.
        let pending = match unsafe { ax_task::current_needs_reschedule_pinned() } {
            Ok(pending) => pending,
            Err(error) => {
                ax_hal::asm::enable_irqs();
                return Err(error);
            }
        };
        if !pending {
            // Keep IRQs disabled. UserContext::run() enters the architecture
            // return path without exposing a kernel IRQ window after this
            // final no-work snapshot.
            return Ok(());
        }

        ax_hal::asm::enable_irqs();
        // The ordinary task entry consumes both request classes. Recheck with
        // IRQs disabled after it returns, like exit_to_user_mode_loop().
        ax_task::schedule_current_cpu()?;
    }
}

/// Validates a public scheduler entry before it can publish task state.
pub(crate) fn validate_schedule_context(
    _origin: ax_task::runtime::RuntimeScheduleOrigin,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::RuntimeStatus;

    let irqs_enabled = ax_hal::asm::irqs_enabled();
    let hard_irq = in_hard_irq();
    if irqs_enabled {
        ax_hal::asm::disable_irqs();
    }
    let state = read_state();
    if irqs_enabled {
        ax_hal::asm::enable_irqs();
    }
    let preempt_depth = current_preempt_depth();
    if irqs_enabled
        && !hard_irq
        && state.irq.is_clear()
        && state.preempt.is_clear()
        && preempt_depth == 0
    {
        RuntimeStatus::Success
    } else {
        RuntimeStatus::UnsafeContext
    }
}

/// Validates an owner-only CpuLocal access against the fixed CPU guard state.
pub(crate) fn validate_owner_cpu_context() -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::RuntimeStatus;

    // Every valid owner scope already disabled raw IRQs before reconstructing
    // the CpuLocal reference. Refuse to create a diagnostic IRQ window here:
    // doing so would itself permit the scheduler re-entry this check prevents.
    if ax_hal::asm::irqs_enabled() {
        return RuntimeStatus::UnsafeContext;
    }
    let state = read_state();
    if in_hard_irq_pinned() && state.irq.is_clear() {
        return RuntimeStatus::UnsafeContext;
    }
    if state.owns_cpu_context() {
        RuntimeStatus::Success
    } else {
        RuntimeStatus::UnsafeContext
    }
}

/// Reports whether the current CPU is in a context that must not sleep.
#[cfg(feature = "fs")]
pub(crate) fn in_atomic_context() -> bool {
    if !ax_hal::asm::irqs_enabled() {
        return true;
    }
    if ax_hal::irq::in_irq_context() {
        return true;
    }

    // A raw local-IRQ window gives a coherent snapshot of preemption nesting
    // without recursively entering ax-kspin's LockRuntime hooks.
    ax_hal::asm::disable_irqs();
    let guarded = read_state().has_context_guard(current_preempt_depth());
    ax_hal::asm::enable_irqs();
    guarded
}
pub(crate) fn enter_irq() {
    let outer_irqs_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    with_guard_state_mut(|state| state.enter_irq(outer_irqs_enabled));
}
pub(crate) fn exit_irq(owner: &'static str) {
    let (must_schedule, restore_irqs) = with_guard_state_mut(|state| {
        let needs_reschedule = state.irq.depth == 1 && {
            // SAFETY: raw IRQ exclusion retains the same CPU while this
            // query observes the current CpuRemote's sticky request.
            unsafe { ax_task::current_needs_immediate_scheduler_work_pinned() }.unwrap_or_else(
                |error| panic!("IRQ guard exit lost the current scheduler owner: {error:?}"),
            )
        };
        if needs_reschedule {
            publish_preemption_pending(true);
        }
        if irq_guard_exit_needs_schedule(state, current_preempt_depth(), in_hard_irq_pinned, || {
            needs_reschedule
        }) {
            (true, false)
        } else {
            (false, state.exit_irq(owner))
        }
    });

    if must_schedule {
        // SAFETY: the final task-context IRQ guard and raw IRQ exclusion stay
        // live until scheduler-frame entry atomically consumes that depth.
        if let Err(error) = unsafe { ax_task::schedule_current_cpu_from_irq_guard_exit() } {
            panic!("IRQ-guard-exit scheduler entry failed: {error}");
        }
        return;
    }

    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(not(any(test, feature = "host-test")))]
pub(crate) fn publish_local_scheduler_work() -> bool {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "local scheduler-work query requires an IRQ publication guard"
    );
    publish_preemption_pending(true);
    in_hard_irq_pinned()
        || read_state().local_scheduler_work_is_self_serviced(current_preempt_depth())
}

#[cfg(all(
    any(test, feature = "host-test"),
    any(feature = "ipi", feature = "wake-ipi")
))]
pub(crate) const fn publish_local_scheduler_work() -> bool {
    false
}
pub(crate) fn finish_initial_context_switch() {
    assert_eq!(
        current_preempt_depth(),
        0,
        "initial scheduler frame must own only the transferred scheduler baton"
    );
    let _task_context_safe = exit_scheduler_frame_guard_inner(
        ax_task::runtime::RuntimeSchedulerReturn::Task,
        "initial scheduler frame",
    );
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreemptExitOrigin {
    Task,
    IrqReturn,
}
impl PreemptExitOrigin {
    const fn is_irq_return(self) -> bool {
        matches!(self, Self::IrqReturn)
    }
}

#[cfg(not(test))]
fn exit_lock_preempt(origin: PreemptExitOrigin, token: cpu_local::PreemptionToken) {
    let irq_return = origin.is_irq_return();
    assert!(
        !irq_return || !ax_hal::asm::irqs_enabled(),
        "IRQ-return preemption exit requires hardware IRQs disabled"
    );
    let cpu_local::PreemptionExit::Pending(pending) = cpu_local::finish_preemption(token) else {
        return;
    };

    // Like Linux's preempt_count_dec_and_test(), only the final pending exit
    // enters the IRQ-excluded scheduling path. The retained depth pins this
    // execution until the scheduler baton or pending.release() consumes it.
    let irqs_were_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    let must_schedule = with_guard_state_mut(|state| {
        let must_schedule = preempt_exit_needs_schedule(
            state,
            current_preempt_depth(),
            origin,
            irqs_were_enabled,
            in_hard_irq_pinned,
        );
        if must_schedule {
            assert!(
                state.claim_preempt_exit_scheduler(current_preempt_depth()),
                "final preemption depth could not become the scheduler baton"
            );
        }
        must_schedule
    });

    // A pending final exit retains depth one until either a CPU-local scheduler
    // baton or a later safe point owns the observation. Releasing it never
    // clears the pending bit; the scheduler tail republishes the authoritative
    // ax-task state after it has processed the current runqueue.
    pending.release();

    if must_schedule {
        use ax_task::runtime::RuntimeSchedulerEntry;

        let entry = match origin {
            PreemptExitOrigin::Task => RuntimeSchedulerEntry::PreemptExit,
            PreemptExitOrigin::IrqReturn => RuntimeSchedulerEntry::IrqReturn,
        };
        // SAFETY: the preclaimed CPU-local baton and raw IRQ exclusion replace
        // the exact final preemption depth without exposing a preemptible gap.
        if let Err(error) = unsafe { ax_task::schedule_current_cpu_from_preempt_exit(entry) } {
            panic!("preemption-exit scheduler entry failed: {error}");
        }
        assert_eq!(
            ax_hal::asm::irqs_enabled(),
            !irq_return,
            "scheduler continuation restored the wrong hardware IRQ state"
        );
        return;
    }

    if !irq_return && irqs_were_enabled {
        ax_hal::asm::enable_irqs();
    }
}

/// Enters an ordinary lock-preemption scope unless a stronger owner scope is active.
///
/// Scheduler frames and runtime IRQ guards already retain this CPU with raw
/// local IRQs disabled. Reusing that ownership matches Linux rq locking: one
/// outer rq/IRQ transaction covers its internal task-state locks, so those
/// locks must not repeatedly mutate the suspended task's preemption word.
#[cfg(not(any(test, feature = "host-test")))]
pub(crate) fn enter_lock_preempt() -> Option<cpu_local::PreemptionToken> {
    if !ax_hal::asm::irqs_enabled() && read_state().owns_cpu_context() {
        return None;
    }
    let token = cpu_local::enter_preemption();
    Some(token)
}

#[cfg(any(test, feature = "host-test"))]
pub(crate) const fn enter_lock_preempt() -> Option<cpu_local::PreemptionToken> {
    None
}

#[cfg(not(test))]
pub(crate) fn exit_preempt(token: cpu_local::PreemptionToken) {
    exit_lock_preempt(PreemptExitOrigin::Task, token);
}

#[cfg(test)]
pub(crate) fn exit_preempt(_token: cpu_local::PreemptionToken) {
    panic!("unit-test runtime cannot exit an unowned preemption guard")
}

#[cfg(not(test))]
pub(crate) fn exit_preempt_from_irq_return(token: cpu_local::PreemptionToken) {
    exit_lock_preempt(PreemptExitOrigin::IrqReturn, token);
}

#[cfg(test)]
pub(crate) fn exit_preempt_from_irq_return(_token: cpu_local::PreemptionToken) {
    panic!("unit-test runtime cannot exit an unowned IRQ-return guard")
}
/// Checks only context constraints after the selected preemption word returned
/// `FinalPending`; that transition is already the reschedule observation.
fn preempt_exit_needs_schedule(
    state: &RuntimeGuardState,
    preempt_depth: u32,
    origin: PreemptExitOrigin,
    irqs_were_enabled: bool,
    in_hard_irq: impl FnOnce() -> bool,
) -> bool {
    state.irq.is_clear()
        && preempt_depth == 1
        && matches!(state.preempt.scheduler_baton, SchedulerBatonState::Finished)
        && (origin.is_irq_return() || irqs_were_enabled)
        && !in_hard_irq()
}
fn irq_guard_exit_needs_schedule(
    state: &RuntimeGuardState,
    preempt_depth: u32,
    in_hard_irq: impl FnOnce() -> bool,
    needs_reschedule: impl FnOnce() -> bool,
) -> bool {
    state.irq.depth == 1
        && state.irq.outer_irqs_enabled
        && preempt_depth == 0
        && state.preempt.is_clear()
        && !in_hard_irq()
        && needs_reschedule()
}
pub(crate) fn enter_scheduler_frame_guard(
    _origin: ax_task::runtime::RuntimeScheduleOrigin,
    entry: ax_task::runtime::RuntimeSchedulerEntry,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::{RuntimeSchedulerEntry, RuntimeStatus};

    let irqs_enabled = ax_hal::asm::irqs_enabled();
    let raw_state_valid = match entry {
        RuntimeSchedulerEntry::Task => irqs_enabled,
        RuntimeSchedulerEntry::PreemptExit
        | RuntimeSchedulerEntry::IrqReturn
        | RuntimeSchedulerEntry::IrqReturnContinuation
        | RuntimeSchedulerEntry::IrqGuardExit => !irqs_enabled,
    };
    if !raw_state_valid || in_hard_irq() {
        return RuntimeStatus::UnsafeContext;
    }

    ax_hal::asm::disable_irqs();
    let preempt_depth = current_preempt_depth();
    let claimed = with_guard_state_mut(|state| match entry {
        RuntimeSchedulerEntry::Task => state.claim_task_scheduler(preempt_depth),
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => {
            state.enter_preclaimed_scheduler(preempt_depth)
        }
        RuntimeSchedulerEntry::IrqReturnContinuation => state.claim_task_scheduler(preempt_depth),
        RuntimeSchedulerEntry::IrqGuardExit => state.claim_irq_exit_scheduler(preempt_depth),
    });
    if !claimed {
        if irqs_enabled {
            ax_hal::asm::enable_irqs();
        }
        return RuntimeStatus::UnsafeContext;
    }
    RuntimeStatus::Success
}
pub(crate) fn exit_scheduler_frame_guard(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
) -> bool {
    exit_scheduler_frame_guard_inner(return_to, "resumed scheduler frame")
}
fn exit_scheduler_frame_guard_inner(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
    owner: &'static str,
) -> bool {
    use ax_task::runtime::RuntimeSchedulerReturn;

    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton must keep hardware IRQs disabled until switch tail"
    );
    let needs_reschedule = {
        // SAFETY: the scheduler baton and raw IRQ exclusion retain this CPU
        // through the current endpoint observation.
        unsafe { ax_task::current_needs_immediate_scheduler_work_pinned() }.unwrap_or_else(
            |error| panic!("scheduler tail lost the current scheduler owner: {error:?}"),
        )
    };
    publish_preemption_pending(needs_reschedule);
    with_guard_state_mut(|state| state.exit_scheduler_preempt(owner));
    crate::clock_event_runtime::finish_deferred_rearm();
    match return_to {
        RuntimeSchedulerReturn::Task => {
            ax_hal::asm::enable_irqs();
            true
        }
        RuntimeSchedulerReturn::IrqReturn => false,
    }
}

/// Verifies the fixed CPU-local baton immediately before the raw switch.
pub(crate) fn assert_scheduler_switch_baton() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler switch requires local IRQs disabled"
    );
    let state = read_state();
    assert!(
        state.irq.is_clear() && state.preempt.has_active_scheduler_baton(),
        "scheduler switch requires the active CPU-local scheduler baton"
    );
}

/// Commits the scheduler baton to the raw context-switch continuation.
pub(crate) fn transfer_scheduler_switch_baton() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton transfer requires local IRQs disabled"
    );
    with_guard_state_mut(RuntimeGuardState::transfer_scheduler_preempt);
}
fn in_hard_irq() -> bool {
    ax_hal::irq::in_irq_context()
}
fn in_hard_irq_pinned() -> bool {
    // SAFETY: every caller has already disabled raw local IRQs or owns the
    // scheduler baton, which prevents migration across this observation.
    unsafe { ax_hal::irq::in_irq_context_pinned() }
}

fn read_state() -> RuntimeGuardState {
    with_guard_state(|state| *state)
}

#[inline(always)]
fn current_preempt_depth() -> u32 {
    with_current_cpu_pin(cpu_local::preemption_snapshot)
        .unwrap_or_else(|error| panic!("architecture preemption state is invalid: {error}"))
        .depth()
}
fn publish_preemption_pending(pending: bool) {
    with_current_cpu_pin(|pin| {
        if pending {
            cpu_local::set_preemption_pending(pin)
        } else {
            cpu_local::clear_preemption_pending(pin)
        }
    })
    .unwrap_or_else(|error| panic!("architecture preemption publication failed: {error}"));
}

fn with_current_cpu_pin<R>(
    operation: impl for<'scope> FnOnce(&cpu_local::CpuPin<'scope>) -> R,
) -> R {
    let restore_irqs = ax_hal::asm::irqs_enabled();
    if restore_irqs {
        ax_hal::asm::disable_irqs();
    }
    // SAFETY: local IRQ exclusion prevents migration for the complete
    // non-escaping CPU-local operation.
    let result = unsafe { cpu_local::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("runtime CPU-local state is invalid: {error}"));
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
    result
}

fn with_guard_state<R>(operation: impl for<'value> FnOnce(&'value RuntimeGuardState) -> R) -> R {
    with_current_cpu_pin(|pin| RUNTIME_GUARD_STATE.with_current(pin, operation))
}
fn with_guard_state_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut RuntimeGuardState) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable runtime guard state requires local IRQ exclusion"
    );
    with_current_cpu_pin(|pin| {
        // SAFETY: local IRQ exclusion prevents migration, re-entry, and every
        // conflicting owner access for the complete callback.
        unsafe {
            cpu_local::with_exclusive_cpu(pin, |exclusive| {
                RUNTIME_GUARD_STATE.with_current_mut(exclusive, operation)
            })
        }
    })
}

#[cfg(test)]
mod tests;
