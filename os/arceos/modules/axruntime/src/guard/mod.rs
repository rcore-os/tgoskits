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
    assert_eq!(
        current_preempt_depth(),
        0,
        "current-thread preemption guard crossed a runtime boot phase"
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
    if irqs_enabled
        && !hard_irq
        && state.irq.is_clear()
        && state.preempt.is_clear()
        && current_preempt_depth() == 0
    {
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
    #[cfg(feature = "irq")]
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

#[cfg(feature = "multitask")]
pub(crate) fn enter_irq() {
    let outer_irqs_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    with_guard_state_mut(|state| state.enter_irq(outer_irqs_enabled));
}

#[cfg(feature = "multitask")]
pub(crate) fn exit_irq(owner: &'static str) {
    let (must_schedule, restore_irqs) = with_guard_state_mut(|state| {
        let needs_reschedule = state.irq.depth == 1 && {
            // SAFETY: raw IRQ exclusion retains the same CPU while this
            // query observes the current CpuRemote's sticky request.
            unsafe { ax_task::current_needs_reschedule_pinned() }.unwrap_or(false)
        };
        if needs_reschedule {
            ax_hal::percpu::scheduler_set_preempt_need_resched()
                .expect("CPU-local preemption state is invalid");
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

#[cfg(all(feature = "multitask", not(test)))]
pub(crate) fn local_scheduler_work_is_self_serviced() -> bool {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "local scheduler-work query requires an IRQ publication guard"
    );
    ax_hal::percpu::scheduler_set_preempt_need_resched()
        .expect("CPU-local preemption state is invalid");
    in_hard_irq_pinned()
        || read_state().local_scheduler_work_is_self_serviced(current_preempt_depth())
}

#[cfg(feature = "multitask")]
pub(crate) fn finish_initial_context_switch() {
    let _task_context_safe = exit_scheduler_frame_guard_inner(
        ax_task::runtime::RuntimeSchedulerReturn::Task,
        "initial scheduler frame",
    );
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
    #[cfg(feature = "multitask")]
    let must_schedule = with_guard_state_mut(|state| {
        let preempt_depth = current_preempt_depth();
        let must_schedule = preempt_exit_needs_schedule(
            state,
            preempt_depth,
            irq_return,
            irqs_were_enabled,
            in_hard_irq_pinned,
        );

        if !must_schedule {
            assert!(
                ax_hal::percpu::scheduler_consume_final_preempt_guard()
                    .expect("CPU-local preemption state is invalid"),
                "final CPU-local preemption guard changed under local IRQ exclusion"
            );
        }
        must_schedule
    });
    #[cfg(not(feature = "multitask"))]
    assert!(
        ax_hal::percpu::scheduler_consume_final_preempt_guard()
            .expect("CPU-local preemption state is invalid"),
        "final CPU-local preemption guard changed under local IRQ exclusion"
    );

    #[cfg(feature = "multitask")]
    {
        use ax_task::runtime::RuntimeSchedulerEntry;

        if must_schedule {
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

    if !irq_return && irqs_were_enabled {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(any(feature = "multitask", test))]
pub(crate) fn enter_preempt() {
    ax_hal::percpu::scheduler_enter_preempt_guard().expect("CPU-local preemption state is invalid");
}

/// Enters an ordinary lock-preemption scope unless a stronger owner scope is active.
///
/// Scheduler frames and runtime IRQ guards already retain this CPU with raw
/// local IRQs disabled. Reusing that ownership matches Linux rq locking: one
/// outer rq/IRQ transaction covers its internal task-state locks, so those
/// locks must not repeatedly mutate the suspended task's preemption word.
#[cfg(feature = "multitask")]
pub(crate) fn enter_lock_preempt() -> bool {
    if !ax_hal::asm::irqs_enabled() && read_state().owns_cpu_context() {
        return false;
    }
    enter_preempt();
    true
}

#[cfg(any(feature = "multitask", test))]
pub(crate) fn exit_preempt() {
    let exit = ax_hal::percpu::scheduler_prepare_preempt_guard_exit()
        .expect("CPU-local preemption state is invalid");
    match exit {
        ax_hal::percpu::CpuPreemptExit::NestedConsumed
        | ax_hal::percpu::CpuPreemptExit::FinalConsumed => {}
        ax_hal::percpu::CpuPreemptExit::FinalPending => {
            exit_lock_preempt(!ax_hal::asm::irqs_enabled());
        }
    }
}

#[cfg(any(feature = "multitask", test))]
/// Checks only context constraints after the CPU preemption word returned
/// `FinalPending`; that transition is already the reschedule observation.
fn preempt_exit_needs_schedule(
    state: &RuntimeGuardState,
    preempt_depth: u32,
    irq_return: bool,
    irqs_were_enabled: bool,
    in_hard_irq: impl FnOnce() -> bool,
) -> bool {
    state.irq.is_clear()
        && preempt_depth == 1
        && matches!(state.preempt.scheduler_baton, SchedulerBatonState::Finished)
        && (irq_return || irqs_were_enabled)
        && !in_hard_irq()
}

#[cfg(any(feature = "multitask", test))]
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

#[cfg(feature = "multitask")]
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
            let mut claimed = *state;
            if !claimed.claim_preempt_exit_scheduler(preempt_depth)
                || !ax_hal::percpu::scheduler_consume_final_preempt_guard()
                    .expect("CPU-local preemption state is invalid")
            {
                false
            } else {
                *state = claimed;
                true
            }
        }
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
    let needs_reschedule = {
        // SAFETY: the scheduler baton and raw IRQ exclusion retain this CPU
        // through the current endpoint observation.
        unsafe { ax_task::current_needs_reschedule_pinned() }.unwrap_or(false)
    };
    let publication = if needs_reschedule {
        ax_hal::percpu::scheduler_set_preempt_need_resched()
    } else {
        ax_hal::percpu::scheduler_clear_preempt_need_resched()
    };
    publication.expect("CPU-local preemption state is invalid");
    with_guard_state_mut(|state| state.exit_scheduler_preempt(owner));
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
    with_guard_state_mut(RuntimeGuardState::transfer_scheduler_preempt);
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

#[cfg(feature = "multitask")]
fn in_hard_irq_pinned() -> bool {
    #[cfg(feature = "irq")]
    {
        // SAFETY: every caller has already disabled raw local IRQs or owns the
        // scheduler baton, which prevents migration across this observation.
        unsafe { ax_hal::irq::in_irq_context_pinned() }
    }
    #[cfg(not(feature = "irq"))]
    {
        false
    }
}

fn read_state() -> RuntimeGuardState {
    with_guard_state(|state| *state)
}

#[inline(always)]
fn current_preempt_depth() -> u32 {
    ax_hal::percpu::scheduler_preempt_guard_depth().expect("CPU-local preemption state is invalid")
}

fn with_guard_state<R>(operation: impl for<'value> FnOnce(&'value RuntimeGuardState) -> R) -> R {
    // SAFETY: callers run on an offline CPU or with raw local IRQ exclusion,
    // so the current thread and every guard-state mutation remain fixed for
    // the complete non-escaping callback.
    unsafe { RUNTIME_GUARD_STATE.with_scheduler_current(operation) }
        .unwrap_or_else(|error| panic!("runtime guard CPU-local state is invalid: {error}"))
}

#[cfg(feature = "multitask")]
fn with_guard_state_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut RuntimeGuardState) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable runtime guard state requires local IRQ exclusion"
    );
    // SAFETY: local IRQ exclusion prevents migration, re-entry, and every
    // conflicting owner access for the complete callback.
    unsafe { RUNTIME_GUARD_STATE.with_scheduler_current_mut(operation) }
        .unwrap_or_else(|error| panic!("runtime guard CPU-local state is invalid: {error}"))
}

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn disable_preempt() {
        if ax_hal::percpu::scheduler_enter_preempt_guard().is_err() {
            #[cfg(not(feature = "host-test"))]
            panic!("CPU-local preemption state is invalid while disabling preemption");
        }
    }

    fn enable_preempt() {
        let Ok(exit) = ax_hal::percpu::scheduler_prepare_preempt_guard_exit() else {
            #[cfg(not(feature = "host-test"))]
            panic!("CPU-local preemption state is invalid while enabling preemption");
            #[cfg(feature = "host-test")]
            return;
        };
        match exit {
            ax_hal::percpu::CpuPreemptExit::NestedConsumed
            | ax_hal::percpu::CpuPreemptExit::FinalConsumed => return,
            ax_hal::percpu::CpuPreemptExit::FinalPending => {}
        }
        let irq_return = !ax_hal::asm::irqs_enabled();
        exit_lock_preempt(irq_return);
    }
}

#[cfg(test)]
mod tests;
