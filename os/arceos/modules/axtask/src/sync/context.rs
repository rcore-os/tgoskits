//! Execution-context guards used by the synchronization primitives.

use core::marker::PhantomData;

/// Saves the local interrupt state and disables local interrupts.
///
/// This low-level entry point exists for capability adapters whose trait API
/// transports the saved interrupt state separately from an RAII guard.
#[doc(hidden)]
#[inline(always)]
pub fn irq_save_and_disable() -> usize {
    imp::irq_save_and_disable()
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
    imp::irq_restore(state);
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

/// Owns an entered critical section until a lock guard takes it over.
///
/// Lockdep validation can panic in host tests. Keeping the state in this
/// temporary guard ensures that every acquisition path restores its execution
/// context while unwinding.
pub(crate) struct PendingGuardState<G: GuardState> {
    state: Option<G::State>,
}

impl<G: GuardState> PendingGuardState<G> {
    #[inline(always)]
    pub(crate) fn acquire() -> Self {
        Self {
            state: Some(G::acquire()),
        }
    }

    #[inline(always)]
    pub(crate) fn into_state(mut self) -> G::State {
        self.state
            .take()
            .expect("pending guard state must be present")
    }
}

impl<G: GuardState> Drop for PendingGuardState<G> {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            G::release(state);
        }
    }
}

/// Raw lock state which does not alter the execution context.
#[doc(hidden)]
pub struct RawState;

/// Lock state which disables kernel preemption.
#[doc(hidden)]
pub struct PreemptState;

impl PreemptState {
    pub(crate) fn release_from_irq_return(state: usize) {
        imp::enable_preempt_from_irq_return(state);
    }
}

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
    type State = usize;

    #[inline(always)]
    fn acquire() -> Self::State {
        imp::disable_preempt()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        imp::enable_preempt(state);
    }

    fn lockdep_enabled() -> bool {
        true
    }
}

impl GuardState for IrqSaveState {
    type State = usize;

    #[inline(always)]
    fn acquire() -> Self::State {
        imp::irq_save_and_disable()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        imp::irq_restore(state);
    }
}

impl GuardState for PreemptIrqSaveState {
    type State = usize;

    #[inline(always)]
    fn acquire() -> Self::State {
        let preemption = imp::disable_preempt();
        assert_eq!(preemption & 1, 0, "preemption token must be aligned");
        preemption | (imp::irq_save_and_disable() & 1)
    }

    #[inline(always)]
    fn release(state: Self::State) {
        imp::irq_restore(state & 1);
        imp::enable_preempt(state & !1);
    }

    fn lockdep_enabled() -> bool {
        true
    }
}

/// An RAII guard which disables kernel preemption while it is alive.
///
/// The guard is bound to the task that acquires it and cannot be transferred
/// to another execution context:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::PreemptGuard>();
/// ```
pub struct PreemptGuard {
    state: <PreemptState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptGuard {
    /// Disables preemption and creates a guard which restores it on drop.
    pub fn new() -> Self {
        Self {
            state: PreemptState::acquire(),
            _not_send: PhantomData,
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
///
/// The saved IRQ state belongs to the acquiring CPU, so the guard cannot be
/// transferred to another execution context:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::IrqSaveGuard>();
/// ```
pub struct IrqSaveGuard {
    state: <IrqSaveState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl IrqSaveGuard {
    /// Saves and disables local interrupts.
    pub fn new() -> Self {
        Self {
            state: IrqSaveState::acquire(),
            _not_send: PhantomData,
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
///
/// Both saved states belong to the acquiring task and CPU, so the guard cannot
/// be transferred to another execution context:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_task::sync::PreemptIrqSaveGuard>();
/// ```
pub struct PreemptIrqSaveGuard {
    state: <PreemptIrqSaveState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptIrqSaveGuard {
    /// Enters a preemption-disabled, IRQ-disabled critical section.
    pub fn new() -> Self {
        Self {
            state: PreemptIrqSaveState::acquire(),
            _not_send: PhantomData,
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
mod imp {
    use std::cell::{Cell, RefCell};

    std::thread_local! {
        static PREEMPT_DEPTH: Cell<usize> = const { Cell::new(0) };
        static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
        static EVENTS: RefCell<std::vec::Vec<&'static str>> = const { RefCell::new(std::vec::Vec::new()) };
    }

    pub(super) fn disable_preempt() -> usize {
        EVENTS.with_borrow_mut(|events| events.push("preempt-disable"));
        PREEMPT_DEPTH.set(PREEMPT_DEPTH.get() + 1);
        0
    }

    pub(super) fn enable_preempt(_state: usize) {
        EVENTS.with_borrow_mut(|events| events.push("preempt-enable"));
        PREEMPT_DEPTH.set(
            PREEMPT_DEPTH
                .get()
                .checked_sub(1)
                .expect("unbalanced preemption guard"),
        );
    }

    pub(super) fn enable_preempt_from_irq_return(state: usize) {
        enable_preempt(state);
    }

    pub(super) fn irq_save_and_disable() -> usize {
        EVENTS.with_borrow_mut(|events| events.push("irq-disable"));
        let was_enabled = IRQ_ENABLED.replace(false);
        usize::from(was_enabled)
    }

    pub(super) fn irq_restore(state: usize) {
        EVENTS.with_borrow_mut(|events| events.push("irq-restore"));
        IRQ_ENABLED.set(state != 0);
    }

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

    #[cfg(feature = "preempt")]
    pub(super) fn finish_initial_context_switch() {
        assert_eq!(
            PREEMPT_DEPTH.get(),
            1,
            "initial host context switch must inherit one exclusion depth"
        );
        PREEMPT_DEPTH.set(0);
    }
}

#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
mod imp {
    #[inline(always)]
    pub(super) fn disable_preempt() -> usize {
        crate::disable_preempt()
    }

    #[inline(always)]
    pub(super) fn enable_preempt(state: usize) {
        crate::enable_preempt(state);
    }

    #[inline(always)]
    pub(super) fn enable_preempt_from_irq_return(state: usize) {
        crate::enable_preempt_from_irq_return(state);
    }

    #[inline(always)]
    pub(super) fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    #[inline(always)]
    pub(super) fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }
}

/// Returns the preemption depth tracked by the host critical-section provider.
#[cfg(all(feature = "host-test", not(target_os = "none")))]
#[doc(hidden)]
pub fn host_preempt_depth() -> usize {
    imp::preempt_depth()
}

#[cfg(all(feature = "preempt", feature = "host-test", not(target_os = "none")))]
pub(crate) fn finish_initial_host_context_switch() {
    imp::finish_initial_context_switch();
}

#[cfg(all(feature = "host-test", not(target_os = "none")))]
pub(crate) fn host_context_snapshot() -> (usize, bool) {
    imp::snapshot()
}

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
mod tests {
    use super::{IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard, imp};

    #[test]
    fn preempt_guard_nests_and_restores_depth() {
        assert_eq!(imp::snapshot(), (0, true));
        let outer = PreemptGuard::new();
        assert_eq!(imp::snapshot(), (1, true));
        {
            let _inner = PreemptGuard::new();
            assert_eq!(imp::snapshot(), (2, true));
        }
        assert_eq!(imp::snapshot(), (1, true));
        drop(outer);
        assert_eq!(imp::snapshot(), (0, true));
    }

    #[test]
    fn irq_save_guard_preserves_nested_disabled_state() {
        assert_eq!(imp::snapshot(), (0, true));
        let outer = IrqSaveGuard::new();
        assert_eq!(imp::snapshot(), (0, false));
        {
            let _inner = IrqSaveGuard::new();
            assert_eq!(imp::snapshot(), (0, false));
        }
        assert_eq!(imp::snapshot(), (0, false));
        drop(outer);
        assert_eq!(imp::snapshot(), (0, true));
    }

    #[test]
    fn combined_guard_restores_irq_before_preempt_context() {
        assert_eq!(imp::snapshot(), (0, true));
        let _ = imp::take_events();
        let guard = PreemptIrqSaveGuard::new();
        assert_eq!(imp::snapshot(), (1, false));
        drop(guard);
        assert_eq!(imp::snapshot(), (0, true));
        assert_eq!(
            imp::take_events(),
            [
                "preempt-disable",
                "irq-disable",
                "irq-restore",
                "preempt-enable"
            ]
        );
    }
}
