//! Execution-context guards used by the synchronization primitives.

/// Runtime operations required to enter and leave kernel critical sections.
///
/// The operating-system runtime provides this interface. Keeping the
/// low-level operations behind this capability lets portable components use
/// [`crate::SpinLock`] without depending on a scheduler or hardware layer.
#[ax_crate_interface::def_interface]
pub trait CriticalSectionOps {
    /// Disables kernel preemption for the current task.
    fn disable_preempt();

    /// Re-enables kernel preemption for the current task.
    fn enable_preempt();

    /// Saves the local interrupt state and disables local interrupts.
    fn irq_save_and_disable() -> usize;

    /// Restores a local interrupt state returned by
    /// [`Self::irq_save_and_disable`].
    fn irq_restore(state: usize);
}

/// Saves the local interrupt state and disables local interrupts.
///
/// This low-level entry point exists for capability adapters whose trait API
/// transports the saved interrupt state separately from an RAII guard.
#[doc(hidden)]
#[inline(always)]
pub fn irq_save_and_disable() -> usize {
    ax_crate_interface::call_interface!(CriticalSectionOps::irq_save_and_disable)
}

/// Restores a local interrupt state returned by [`irq_save_and_disable`].
///
/// # Safety
///
/// `state` must come from the matching save operation on the current CPU and
/// must be restored exactly once, in properly nested order.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn irq_restore(state: usize) {
    ax_crate_interface::call_interface!(CriticalSectionOps::irq_restore, state);
}

/// Internal critical-section contract used by spin-lock guards.
#[doc(hidden)]
pub trait GuardState {
    /// Saved state needed when the guard is released.
    type State: Clone + Copy;

    /// Enters the critical section.
    fn acquire() -> Self::State;

    /// Leaves the critical section.
    fn release(state: Self::State);

    /// Returns whether locks using this state participate in task lockdep.
    fn lockdep_enabled() -> bool {
        false
    }
}

/// Raw lock state which does not alter the execution context.
#[doc(hidden)]
pub struct RawState;

/// Lock state which disables kernel preemption.
#[doc(hidden)]
pub struct PreemptState;

/// Lock state which saves and disables local interrupts.
#[doc(hidden)]
pub struct IrqSaveState;

/// Lock state which disables preemption, then saves and disables interrupts.
#[doc(hidden)]
pub struct PreemptIrqSaveState;

impl GuardState for RawState {
    type State = ();

    #[inline(always)]
    fn acquire() -> Self::State {}

    #[inline(always)]
    fn release(_state: Self::State) {}
}

impl GuardState for PreemptState {
    type State = ();

    #[inline(always)]
    fn acquire() -> Self::State {
        ax_crate_interface::call_interface!(CriticalSectionOps::disable_preempt);
    }

    #[inline(always)]
    fn release(_state: Self::State) {
        ax_crate_interface::call_interface!(CriticalSectionOps::enable_preempt);
    }

    fn lockdep_enabled() -> bool {
        true
    }
}

impl GuardState for IrqSaveState {
    type State = usize;

    #[inline(always)]
    fn acquire() -> Self::State {
        ax_crate_interface::call_interface!(CriticalSectionOps::irq_save_and_disable)
    }

    #[inline(always)]
    fn release(state: Self::State) {
        ax_crate_interface::call_interface!(CriticalSectionOps::irq_restore, state);
    }
}

impl GuardState for PreemptIrqSaveState {
    type State = usize;

    #[inline(always)]
    fn acquire() -> Self::State {
        ax_crate_interface::call_interface!(CriticalSectionOps::disable_preempt);
        ax_crate_interface::call_interface!(CriticalSectionOps::irq_save_and_disable)
    }

    #[inline(always)]
    fn release(state: Self::State) {
        ax_crate_interface::call_interface!(CriticalSectionOps::irq_restore, state);
        ax_crate_interface::call_interface!(CriticalSectionOps::enable_preempt);
    }

    fn lockdep_enabled() -> bool {
        true
    }
}

/// An RAII guard which disables kernel preemption while it is alive.
pub struct PreemptGuard;

impl PreemptGuard {
    /// Disables preemption and creates a guard which restores it on drop.
    pub fn new() -> Self {
        PreemptState::acquire();
        Self
    }
}

impl Default for PreemptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        PreemptState::release(());
    }
}

/// An RAII guard which saves and disables local interrupts while it is alive.
pub struct IrqSaveGuard {
    state: <IrqSaveState as GuardState>::State,
}

impl IrqSaveGuard {
    /// Saves and disables local interrupts.
    pub fn new() -> Self {
        Self {
            state: IrqSaveState::acquire(),
        }
    }
}

impl Default for IrqSaveGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrqSaveGuard {
    fn drop(&mut self) {
        IrqSaveState::release(self.state);
    }
}

/// An RAII guard which disables preemption and local interrupts.
///
/// Entry disables preemption before interrupts. Drop restores interrupts
/// before re-enabling preemption, matching Linux spin-lock IRQ-save ordering.
pub struct PreemptIrqSaveGuard {
    state: <PreemptIrqSaveState as GuardState>::State,
}

impl PreemptIrqSaveGuard {
    /// Enters a preemption-disabled, IRQ-disabled critical section.
    pub fn new() -> Self {
        Self {
            state: PreemptIrqSaveState::acquire(),
        }
    }
}

impl Default for PreemptIrqSaveGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptIrqSaveGuard {
    fn drop(&mut self) {
        PreemptIrqSaveState::release(self.state);
    }
}

#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host {
    use std::cell::{Cell, RefCell};

    use super::CriticalSectionOps;

    std::thread_local! {
        static PREEMPT_DEPTH: Cell<usize> = const { Cell::new(0) };
        static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
        static EVENTS: RefCell<std::vec::Vec<&'static str>> = const { RefCell::new(std::vec::Vec::new()) };
    }

    struct HostCriticalSectionOps;

    #[ax_crate_interface::impl_interface]
    impl CriticalSectionOps for HostCriticalSectionOps {
        fn disable_preempt() {
            EVENTS.with_borrow_mut(|events| events.push("preempt-disable"));
            PREEMPT_DEPTH.set(PREEMPT_DEPTH.get() + 1);
        }

        fn enable_preempt() {
            EVENTS.with_borrow_mut(|events| events.push("preempt-enable"));
            PREEMPT_DEPTH.set(
                PREEMPT_DEPTH
                    .get()
                    .checked_sub(1)
                    .expect("unbalanced preemption guard"),
            );
        }

        fn irq_save_and_disable() -> usize {
            EVENTS.with_borrow_mut(|events| events.push("irq-disable"));
            let was_enabled = IRQ_ENABLED.replace(false);
            usize::from(was_enabled)
        }

        fn irq_restore(state: usize) {
            EVENTS.with_borrow_mut(|events| events.push("irq-restore"));
            IRQ_ENABLED.set(state != 0);
        }
    }

    #[cfg(all(test, feature = "host-test", not(target_os = "none")))]
    pub(super) fn snapshot() -> (usize, bool) {
        (PREEMPT_DEPTH.get(), IRQ_ENABLED.get())
    }

    #[cfg(all(test, feature = "host-test", not(target_os = "none")))]
    pub(super) fn take_events() -> std::vec::Vec<&'static str> {
        EVENTS.take()
    }

    pub(super) fn preempt_depth() -> usize {
        PREEMPT_DEPTH.get()
    }
}

/// Returns the preemption depth tracked by the host critical-section provider.
#[cfg(all(feature = "host-test", not(target_os = "none")))]
#[doc(hidden)]
pub fn host_preempt_depth() -> usize {
    host::preempt_depth()
}

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
pub(crate) fn host_context_snapshot() -> (usize, bool) {
    host::snapshot()
}

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
mod tests {
    use super::{IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard, host};

    #[test]
    fn preempt_guard_nests_and_restores_depth() {
        assert_eq!(host::snapshot(), (0, true));
        let outer = PreemptGuard::new();
        assert_eq!(host::snapshot(), (1, true));
        {
            let _inner = PreemptGuard::new();
            assert_eq!(host::snapshot(), (2, true));
        }
        assert_eq!(host::snapshot(), (1, true));
        drop(outer);
        assert_eq!(host::snapshot(), (0, true));
    }

    #[test]
    fn irq_save_guard_preserves_nested_disabled_state() {
        assert_eq!(host::snapshot(), (0, true));
        let outer = IrqSaveGuard::new();
        assert_eq!(host::snapshot(), (0, false));
        {
            let _inner = IrqSaveGuard::new();
            assert_eq!(host::snapshot(), (0, false));
        }
        assert_eq!(host::snapshot(), (0, false));
        drop(outer);
        assert_eq!(host::snapshot(), (0, true));
    }

    #[test]
    fn combined_guard_restores_irq_before_preempt_context() {
        assert_eq!(host::snapshot(), (0, true));
        let _ = host::take_events();
        let guard = PreemptIrqSaveGuard::new();
        assert_eq!(host::snapshot(), (1, false));
        drop(guard);
        assert_eq!(host::snapshot(), (0, true));
        assert_eq!(
            host::take_events(),
            [
                "preempt-disable",
                "irq-disable",
                "irq-restore",
                "preempt-enable"
            ]
        );
    }
}
