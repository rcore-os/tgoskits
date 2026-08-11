//! Execution-context guards used by the synchronization primitives.

/// Opaque runtime ownership returned by a preemption-guard entry.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreemptGuardToken(usize);

impl PreemptGuardToken {
    /// Creates the capability result for one runtime guard entry.
    #[doc(hidden)]
    pub const fn from_entered(entered: bool) -> Self {
        Self(entered as usize)
    }

    /// Returns whether a stronger runtime scope already owns the CPU.
    #[doc(hidden)]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Runtime operations required to enter and leave kernel critical sections.
///
/// The operating-system runtime provides this interface. Keeping the
/// low-level operations behind this capability lets portable components use
/// [`crate::SpinLock`] without depending on a scheduler or hardware layer.
#[ax_crate_interface::def_interface]
pub trait CriticalSectionOps {
    /// Enters a task-preemption exclusion scope.
    ///
    /// Zero means a stronger runtime owner already pins this CPU. Any non-zero
    /// token must be returned unchanged to the matching exit operation.
    fn preempt_guard_enter() -> PreemptGuardToken;

    /// Leaves an ordinary task-preemption exclusion scope.
    fn preempt_guard_exit(token: PreemptGuardToken);

    /// Leaves preemption at an explicit hard-IRQ return boundary.
    ///
    /// The runtime may enter the scheduler while hardware IRQs remain
    /// disabled, but must return with them disabled for the exception epilogue.
    fn preempt_guard_exit_irq_return(token: PreemptGuardToken);

    /// Publishes entry into hard-interrupt accounting and nesting.
    fn hardirq_enter();

    /// Publishes exit from hard-interrupt accounting and nesting.
    fn hardirq_exit();

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
    type State = PreemptGuardToken;

    #[inline(always)]
    fn acquire() -> Self::State {
        ax_crate_interface::call_interface!(CriticalSectionOps::preempt_guard_enter)
    }

    #[inline(always)]
    fn release(state: Self::State) {
        ax_crate_interface::call_interface!(CriticalSectionOps::preempt_guard_exit, state);
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
    type State = (PreemptGuardToken, usize);

    #[inline(always)]
    fn acquire() -> Self::State {
        let preempt = ax_crate_interface::call_interface!(CriticalSectionOps::preempt_guard_enter);
        let irq = ax_crate_interface::call_interface!(CriticalSectionOps::irq_save_and_disable);
        (preempt, irq)
    }

    #[inline(always)]
    fn release((preempt, irq): Self::State) {
        ax_crate_interface::call_interface!(CriticalSectionOps::irq_restore, irq);
        ax_crate_interface::call_interface!(CriticalSectionOps::preempt_guard_exit, preempt);
    }

    fn lockdep_enabled() -> bool {
        true
    }
}

/// An RAII guard which disables kernel preemption while it is alive.
pub struct PreemptGuard {
    state: <PreemptState as GuardState>::State,
}

impl PreemptGuard {
    /// Disables preemption and creates a guard which restores it on drop.
    pub fn new() -> Self {
        Self {
            state: PreemptState::acquire(),
        }
    }
}

impl Default for PreemptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        PreemptState::release(self.state);
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

    /// Disables preemption for work completed by a hard-IRQ return epilogue.
    ///
    /// The mutable borrow prevents local IRQ restoration before the dedicated
    /// preemption exit has completed.
    pub fn disable_preempt_for_irq_return(&mut self) -> IrqReturnPreemptGuard<'_> {
        IrqReturnPreemptGuard {
            token: ax_crate_interface::call_interface!(CriticalSectionOps::preempt_guard_enter),
            _irq_guard: core::marker::PhantomData,
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

/// A preemption guard whose final release is an explicit IRQ-return boundary.
#[must_use = "dropping the guard completes the IRQ-return preemption exit"]
pub struct IrqReturnPreemptGuard<'irq> {
    token: PreemptGuardToken,
    _irq_guard: core::marker::PhantomData<&'irq mut IrqSaveGuard>,
}

impl Drop for IrqReturnPreemptGuard<'_> {
    fn drop(&mut self) {
        ax_crate_interface::call_interface!(
            CriticalSectionOps::preempt_guard_exit_irq_return,
            self.token
        );
    }
}

/// Publishes entry into the runtime's hard-interrupt lifecycle.
#[inline(always)]
pub fn hardirq_enter() {
    ax_crate_interface::call_interface!(CriticalSectionOps::hardirq_enter);
}

/// Publishes exit from the runtime's hard-interrupt lifecycle.
#[inline(always)]
pub fn hardirq_exit() {
    ax_crate_interface::call_interface!(CriticalSectionOps::hardirq_exit);
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
        fn preempt_guard_enter() -> super::PreemptGuardToken {
            EVENTS.with_borrow_mut(|events| events.push("preempt-disable"));
            PREEMPT_DEPTH.set(PREEMPT_DEPTH.get() + 1);
            super::PreemptGuardToken::from_entered(true)
        }

        fn preempt_guard_exit(token: super::PreemptGuardToken) {
            assert!(!token.is_none(), "host preemption token is invalid");
            EVENTS.with_borrow_mut(|events| events.push("preempt-enable"));
            PREEMPT_DEPTH.set(
                PREEMPT_DEPTH
                    .get()
                    .checked_sub(1)
                    .expect("unbalanced preemption guard"),
            );
        }

        fn preempt_guard_exit_irq_return(token: super::PreemptGuardToken) {
            assert!(
                !token.is_none(),
                "host IRQ-return preemption token is invalid"
            );
            EVENTS.with_borrow_mut(|events| events.push("preempt-irq-return"));
            PREEMPT_DEPTH.set(
                PREEMPT_DEPTH
                    .get()
                    .checked_sub(1)
                    .expect("unbalanced IRQ-return preemption guard"),
            );
        }

        fn hardirq_enter() {
            EVENTS.with_borrow_mut(|events| events.push("hardirq-enter"));
        }

        fn hardirq_exit() {
            EVENTS.with_borrow_mut(|events| events.push("hardirq-exit"));
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
    use super::{
        IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard, hardirq_enter, hardirq_exit, host,
    };

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

    #[test]
    fn irq_return_preserves_hardirq_accounting_and_release_order() {
        assert_eq!(host::snapshot(), (0, true));
        let _ = host::take_events();
        let mut irq = IrqSaveGuard::new();
        let preempt = irq.disable_preempt_for_irq_return();
        hardirq_enter();
        hardirq_exit();
        drop(preempt);
        assert_eq!(host::snapshot(), (0, false));
        drop(irq);
        assert_eq!(host::snapshot(), (0, true));
        assert_eq!(
            host::take_events(),
            [
                "irq-disable",
                "preempt-disable",
                "hardirq-enter",
                "hardirq-exit",
                "preempt-irq-return",
                "irq-restore"
            ]
        );
    }
}
