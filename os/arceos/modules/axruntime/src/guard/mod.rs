//! CPU-local implementation of the lock context runtime.

mod state;

#[cfg(any(feature = "multitask", test))]
use state::SchedulerBatonState;
use state::{RuntimeGuardState, RuntimeIrqState, RuntimePreemptState};

#[ax_percpu::def_percpu]
static RUNTIME_GUARD_STATE: RuntimeGuardState = RuntimeGuardState::new();

pub(crate) fn assert_boot_guards_released() {
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
}

/// Validates a public scheduler entry before it can publish task state.
#[cfg(feature = "multitask")]
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
    if irqs_enabled && !hard_irq && state.irq.is_clear() && state.preempt.is_clear() {
        RuntimeStatus::Success
    } else {
        RuntimeStatus::UnsafeContext
    }
}

/// Validates an owner-only CpuLocal access against the fixed CPU guard state.
#[cfg(feature = "multitask")]
pub(crate) fn validate_owner_cpu_context() -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::RuntimeStatus;

    // Every valid owner scope already disabled raw IRQs before reconstructing
    // the CpuLocal reference. Refuse to create a diagnostic IRQ window here:
    // doing so would itself permit the scheduler re-entry this check prevents.
    if ax_hal::asm::irqs_enabled() || (in_hard_irq() && read_state().irq.is_clear()) {
        return RuntimeStatus::UnsafeContext;
    }
    if read_state().owns_cpu_context() {
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
    #[cfg(feature = "irq")]
    if ax_hal::irq::in_irq_context() {
        return true;
    }

    // A raw local-IRQ window gives a coherent snapshot of preemption nesting
    // without recursively entering ax-kspin's LockRuntime hooks.
    ax_hal::asm::disable_irqs();
    let guarded = read_state().has_context_guard();
    ax_hal::asm::enable_irqs();
    guarded
}

#[cfg(feature = "multitask")]
pub(crate) fn enter_irq() {
    let outer_irqs_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    let mut state = read_state();
    state.enter_irq(outer_irqs_enabled);
    write_state(state);
}

#[cfg(feature = "multitask")]
pub(crate) fn exit_irq(owner: &'static str) {
    let mut state = read_state();
    let restore_irqs = state.exit_irq(owner);
    write_state(state);

    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(feature = "multitask")]
pub(crate) fn finish_initial_context_switch() {
    let _task_context_safe = exit_scheduler_frame_guard_inner(
        ax_task::runtime::RuntimeSchedulerReturn::Task,
        "initial scheduler frame",
    );
}

fn update_preempt_state(operation: impl FnOnce(&mut RuntimeGuardState)) {
    // This raw IRQ window serializes the whole per-CPU state update against a
    // hard interrupt. It cannot use ax-kspin because this is its runtime hook.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    operation(&mut state);
    write_state(state);
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
}

fn exit_lock_preempt(irq_return: bool) {
    let irqs_were_enabled = ax_hal::asm::irqs_enabled();
    assert!(
        !irq_return || !irqs_were_enabled,
        "IRQ-return preemption exit requires hardware IRQs disabled"
    );

    // Serialize the eligibility decision against hard IRQ entry. When the last
    // guard must schedule, keep that exact depth published until TaskRuntime
    // atomically converts it into the CPU-local scheduler baton.
    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    #[cfg(feature = "multitask")]
    {
        use ax_task::runtime::RuntimeSchedulerEntry;

        let must_schedule = state.irq.is_clear()
            && state.preempt.lock_depth == 1
            && matches!(state.preempt.scheduler_baton, SchedulerBatonState::Finished)
            && (irq_return || irqs_were_enabled)
            && !in_hard_irq()
            && ax_task::current_cpu_needs_resched().unwrap_or(false);
        if must_schedule {
            write_state(state);
            let entry = if irq_return {
                RuntimeSchedulerEntry::IrqReturn
            } else {
                RuntimeSchedulerEntry::PreemptExit
            };
            // SAFETY: this path retains exactly one lock-preemption depth and
            // keeps raw IRQs disabled while the runtime atomically transforms
            // that depth into the typed scheduler baton.
            if let Err(error) = unsafe { ax_task::schedule_current_cpu_from_preempt_exit(entry) } {
                panic!("preemption-exit scheduler entry failed: {error}");
            }
            return;
        }
    }

    state.exit_lock_preempt();
    write_state(state);
    if !irq_return && irqs_were_enabled {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(feature = "multitask")]
pub(crate) fn enter_scheduler_frame_guard(
    _origin: ax_task::runtime::RuntimeScheduleOrigin,
    entry: ax_task::runtime::RuntimeSchedulerEntry,
) -> ax_task::runtime::RuntimeStatus {
    use ax_task::runtime::{RuntimeSchedulerEntry, RuntimeStatus};

    let irqs_enabled = ax_hal::asm::irqs_enabled();
    let raw_state_valid = match entry {
        RuntimeSchedulerEntry::Task => irqs_enabled,
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => !irqs_enabled,
    };
    if !raw_state_valid || in_hard_irq() {
        return RuntimeStatus::UnsafeContext;
    }

    ax_hal::asm::disable_irqs();
    let mut state = read_state();
    let claimed = match entry {
        RuntimeSchedulerEntry::Task => state.claim_task_scheduler(),
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => {
            state.claim_preempt_exit_scheduler()
        }
    };
    if !claimed {
        if irqs_enabled {
            ax_hal::asm::enable_irqs();
        }
        return RuntimeStatus::UnsafeContext;
    }
    write_state(state);
    RuntimeStatus::Success
}

#[cfg(feature = "multitask")]
pub(crate) fn exit_scheduler_frame_guard(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
) -> bool {
    exit_scheduler_frame_guard_inner(return_to, "resumed scheduler frame")
}

#[cfg(feature = "multitask")]
fn exit_scheduler_frame_guard_inner(
    return_to: ax_task::runtime::RuntimeSchedulerReturn,
    owner: &'static str,
) -> bool {
    use ax_task::runtime::RuntimeSchedulerReturn;

    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton must keep hardware IRQs disabled until switch tail"
    );
    let mut state = read_state();
    state.exit_scheduler_preempt(owner);
    write_state(state);
    match return_to {
        RuntimeSchedulerReturn::Task => {
            ax_hal::asm::enable_irqs();
            true
        }
        RuntimeSchedulerReturn::IrqReturn => false,
    }
}

/// Verifies the fixed CPU-local baton immediately before the raw switch.
#[cfg(feature = "multitask")]
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
#[cfg(feature = "multitask")]
pub(crate) fn transfer_scheduler_switch_baton() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "scheduler baton transfer requires local IRQs disabled"
    );
    let mut state = read_state();
    state.transfer_scheduler_preempt();
    write_state(state);
}

#[cfg(feature = "multitask")]
fn in_hard_irq() -> bool {
    #[cfg(feature = "irq")]
    {
        ax_hal::irq::in_irq_context()
    }
    #[cfg(not(feature = "irq"))]
    {
        false
    }
}

fn read_state() -> RuntimeGuardState {
    with_guard_state(|state| *state)
}

fn write_state(state: RuntimeGuardState) {
    with_guard_state_mut(|current| *current = state);
}

fn with_guard_state<R>(operation: impl for<'value> FnOnce(&'value RuntimeGuardState) -> R) -> R {
    // SAFETY: callers either run before scheduler publication or retain an IRQ
    // or preemption depth for the complete non-escaping callback.
    unsafe { ax_percpu::with_cpu_pin(|pin| RUNTIME_GUARD_STATE.with_current(pin, operation)) }
        .unwrap_or_else(|error| panic!("runtime guard CPU-local state is invalid: {error}"))
}

fn with_guard_state_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut RuntimeGuardState) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable runtime guard state requires local IRQ exclusion"
    );
    // SAFETY: local IRQ exclusion prevents migration, re-entry, and every
    // conflicting owner access for the complete nested callbacks.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                RUNTIME_GUARD_STATE.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("runtime guard CPU-local state is invalid: {error}"))
}

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn disable_preempt() {
        update_preempt_state(RuntimeGuardState::enter_lock_preempt);
    }

    fn enable_preempt() {
        let irq_return = !ax_hal::asm::irqs_enabled();
        exit_lock_preempt(irq_return);
    }
}

#[cfg(test)]
mod tests;
