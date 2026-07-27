//! CPU-local implementation of the lock context runtime.

#[derive(Clone, Copy, Debug)]
struct RuntimeGuardState {
    irq: RuntimeIrqState,
    preempt: RuntimePreemptState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeIrqState {
    depth: u32,
    outer_irqs_enabled: bool,
}

impl RuntimeIrqState {
    const fn new() -> Self {
        Self {
            depth: 0,
            outer_irqs_enabled: false,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    const fn is_clear(self) -> bool {
        self.depth == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimePreemptState {
    lock_depth: u32,
    scheduler_baton: SchedulerBatonState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerBatonState {
    #[cfg(any(feature = "multitask", test))]
    Active,
    #[cfg(any(feature = "multitask", test))]
    Transferred,
    Finished,
}

impl RuntimePreemptState {
    const fn new() -> Self {
        Self {
            lock_depth: 0,
            scheduler_baton: SchedulerBatonState::Finished,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    const fn is_clear(self) -> bool {
        self.lock_depth == 0 && matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    const fn has_one_scheduler_frame(self) -> bool {
        self.lock_depth == 0 && !matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    const fn has_active_scheduler_baton(self) -> bool {
        self.lock_depth == 0 && matches!(self.scheduler_baton, SchedulerBatonState::Active)
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_task_scheduler(&mut self) -> bool {
        if !self.is_clear() {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_preempt_exit_scheduler(&mut self) -> bool {
        if self.lock_depth != 1 || !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.lock_depth = 0;
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    fn transfer_scheduler_baton(&mut self) {
        assert!(
            self.has_active_scheduler_baton(),
            "scheduler baton transfer requires the active scheduler frame"
        );
        self.scheduler_baton = SchedulerBatonState::Transferred;
    }

    #[cfg(any(feature = "multitask", test))]
    fn finish_scheduler_baton(&mut self) {
        assert!(
            self.has_one_scheduler_frame(),
            "scheduler baton finish requires an active or transferred frame"
        );
        self.scheduler_baton = SchedulerBatonState::Finished;
    }
}

impl RuntimeGuardState {
    const fn new() -> Self {
        Self {
            irq: RuntimeIrqState::new(),
            preempt: RuntimePreemptState::new(),
        }
    }

    #[cfg(any(feature = "multitask", test))]
    fn enter_irq(&mut self, outer_irqs_enabled: bool) {
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = outer_irqs_enabled;
        }
        self.irq.depth = self
            .irq
            .depth
            .checked_add(1)
            .expect("runtime IRQ guard nesting overflow");
    }

    #[cfg(any(feature = "multitask", test))]
    fn exit_irq(&mut self, owner: &'static str) -> bool {
        assert!(
            self.irq.depth > 0,
            "unbalanced runtime IRQ guard exit from {owner}"
        );
        self.irq.depth -= 1;
        let restore_irqs = self.irq.depth == 0 && self.irq.outer_irqs_enabled;
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = false;
        }
        restore_irqs
    }

    fn enter_lock_preempt(&mut self) {
        self.preempt.lock_depth = self
            .preempt
            .lock_depth
            .checked_add(1)
            .expect("runtime lock preemption guard nesting overflow");
    }

    fn exit_lock_preempt(&mut self) {
        assert!(
            self.preempt.lock_depth > 0,
            "unbalanced runtime lock preemption guard exit"
        );
        self.preempt.lock_depth -= 1;
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_task_scheduler(&mut self) -> bool {
        self.irq.is_clear() && self.preempt.claim_task_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    fn claim_preempt_exit_scheduler(&mut self) -> bool {
        self.irq.is_clear() && self.preempt.claim_preempt_exit_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    fn exit_scheduler_preempt(&mut self, owner: &'static str) {
        assert!(
            self.irq.is_clear(),
            "{owner} exited with live IRQ guard depth={}, outer_enabled={}",
            self.irq.depth,
            self.irq.outer_irqs_enabled,
        );
        assert!(
            self.preempt.has_one_scheduler_frame(),
            "scheduler frame exit requires the exact scheduler-owned baton"
        );
        self.preempt.finish_scheduler_baton();
    }

    #[cfg(any(feature = "multitask", test))]
    fn transfer_scheduler_preempt(&mut self) {
        assert!(
            self.irq.is_clear(),
            "scheduler baton transferred with a live IRQ guard"
        );
        self.preempt.transfer_scheduler_baton();
    }

    #[cfg(feature = "fs")]
    const fn has_context_guard(self) -> bool {
        !self.irq.is_clear() || !self.preempt.is_clear()
    }
}

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
mod tests {
    use super::*;

    #[test]
    fn nested_irq_exits_restore_only_the_outer_state() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);
        state.enter_irq(false);

        assert!(!state.exit_irq("test"));
        assert!(state.exit_irq("test"));
    }

    #[test]
    fn disabled_outer_irq_state_stays_disabled() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(false);

        assert!(!state.exit_irq("test"));
    }

    #[test]
    fn lock_preempt_exit_reports_only_the_outermost_transition() {
        let mut state = RuntimeGuardState::new();
        state.enter_lock_preempt();
        state.enter_lock_preempt();

        state.exit_lock_preempt();
        assert_eq!(state.preempt.lock_depth, 1);
        state.exit_lock_preempt();
        assert!(state.preempt.is_clear());
    }

    #[test]
    fn scheduler_baton_is_exactly_one_cpu_local_frame() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());
        assert!(state.preempt.has_one_scheduler_frame());
        assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Active);

        state.transfer_scheduler_preempt();
        assert_eq!(
            state.preempt.scheduler_baton,
            SchedulerBatonState::Transferred
        );

        state.exit_scheduler_preempt("test scheduler frame");
        assert!(state.preempt.is_clear());
        assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Finished);
    }

    #[test]
    #[should_panic(expected = "unbalanced runtime lock preemption guard exit")]
    fn lock_exit_cannot_consume_a_scheduler_frame() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());

        state.exit_lock_preempt();
    }

    #[test]
    fn scheduler_frame_cannot_cross_a_live_lock_guard() {
        let mut state = RuntimeGuardState::new();
        state.enter_lock_preempt();

        assert!(!state.claim_task_scheduler());
        assert!(state.claim_preempt_exit_scheduler());
    }

    #[test]
    fn scheduler_frame_cannot_enter_inside_an_ordinary_irq_guard() {
        let mut state = RuntimeGuardState::new();
        state.enter_irq(true);

        assert!(!state.claim_task_scheduler());
    }

    #[test]
    #[should_panic(expected = "test scheduler frame exited with live IRQ guard depth=1")]
    fn scheduler_frame_cannot_cross_a_live_irq_guard() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());
        state.enter_irq(true);

        state.exit_scheduler_preempt("test scheduler frame");
    }

    #[test]
    #[cfg(feature = "fs")]
    fn context_guard_state_rejects_sleep_until_every_depth_is_released() {
        let mut state = RuntimeGuardState::new();
        assert!(!state.has_context_guard());

        state.enter_lock_preempt();
        assert!(state.has_context_guard());
        state.exit_lock_preempt();
        assert!(!state.has_context_guard());
    }

    #[test]
    fn initial_context_entry_consumes_the_scheduler_baton() {
        let mut state = RuntimeGuardState::new();
        assert!(state.claim_task_scheduler());

        state.exit_scheduler_preempt("test scheduler frame");
        assert!(state.preempt.is_clear());
    }
}
