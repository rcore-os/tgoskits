//! Task-owned execution-context guards for synchronization primitives.

use core::marker::PhantomData;

use crate::runtime::{LocalIrqState, PreemptGuardToken, task_runtime};

/// Internal critical-section contract shared by task-owned lock algorithms.
pub(crate) trait GuardState {
    type State: Copy;

    fn acquire() -> Self::State;

    fn release(state: Self::State);
}

struct PendingGuardState<G: GuardState> {
    state: Option<G::State>,
}

impl<G: GuardState> PendingGuardState<G> {
    fn acquire() -> Self {
        Self {
            state: Some(G::acquire()),
        }
    }

    fn into_state(mut self) -> G::State {
        self.state
            .take()
            .expect("pending context state must be owned")
    }
}

impl<G: GuardState> Drop for PendingGuardState<G> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            G::release(state);
        }
    }
}

pub(crate) struct PreemptState;
pub(crate) struct IrqSaveState;
pub(crate) struct PreemptIrqSaveState;

impl GuardState for PreemptState {
    type State = PreemptGuardToken;

    #[inline(always)]
    fn acquire() -> Self::State {
        task_runtime::preempt_guard_enter()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        if state.is_none() {
            return;
        }
        // SAFETY: acquire returned this same-context token, and the owning
        // guard consumes it exactly once without permitting migration.
        unsafe { task_runtime::preempt_guard_exit(state) };
    }
}

impl GuardState for IrqSaveState {
    type State = LocalIrqState;

    #[inline(always)]
    fn acquire() -> Self::State {
        task_runtime::local_irq_save_and_disable()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        // SAFETY: acquire returned this state on the current CPU. The guard is
        // !Send and consumes it exactly once in properly nested order.
        unsafe { task_runtime::local_irq_restore(state) };
    }
}

impl GuardState for PreemptIrqSaveState {
    type State = (PreemptGuardToken, LocalIrqState);

    #[inline(always)]
    fn acquire() -> Self::State {
        let preempt = PendingGuardState::<PreemptState>::acquire();
        let irq = IrqSaveState::acquire();
        (preempt.into_state(), irq)
    }

    #[inline(always)]
    fn release((preempt, irq): Self::State) {
        IrqSaveState::release(irq);
        PreemptState::release(preempt);
    }
}

/// An RAII guard which disables kernel preemption while it is alive.
pub struct PreemptGuard {
    state: <PreemptState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptGuard {
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
pub struct IrqSaveGuard {
    state: <IrqSaveState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl IrqSaveGuard {
    pub fn new() -> Self {
        Self {
            state: IrqSaveState::acquire(),
            _not_send: PhantomData,
        }
    }

    /// Disables preemption for work completed by a hard-IRQ return epilogue.
    ///
    /// The mutable borrow prevents raw local-IRQ restoration before the
    /// dedicated preemption exit has completed.
    pub fn disable_preempt_for_irq_return(&mut self) -> IrqReturnPreemptGuard<'_> {
        IrqReturnPreemptGuard {
            token: task_runtime::preempt_guard_enter(),
            _irq_guard: PhantomData,
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

/// A preemption guard whose final release is an explicit IRQ-return boundary.
#[must_use = "dropping the guard completes the IRQ-return preemption exit"]
pub struct IrqReturnPreemptGuard<'irq> {
    token: PreemptGuardToken,
    _irq_guard: PhantomData<&'irq mut IrqSaveGuard>,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for IrqReturnPreemptGuard<'_> {
    fn drop(&mut self) {
        if self.token.is_none() {
            return;
        }
        // SAFETY: construction received this token on the current execution
        // context, and the !Send guard consumes it exactly once while its raw
        // IRQ guard remains borrowed and active.
        unsafe { task_runtime::preempt_guard_exit_irq_return(self.token) };
    }
}

/// Publishes entry into the runtime's hard-interrupt lifecycle.
#[inline(always)]
pub fn hardirq_enter() {
    task_runtime::hardirq_enter();
}

/// Publishes exit from the runtime's hard-interrupt lifecycle.
#[inline(always)]
pub fn hardirq_exit() {
    task_runtime::hardirq_exit();
}

/// An RAII guard which disables preemption and local interrupts.
///
/// Entry disables preemption before interrupts. Drop restores interrupts
/// before re-enabling preemption, matching Linux spin-lock IRQ-save ordering.
pub struct PreemptIrqSaveGuard {
    state: <PreemptIrqSaveState as GuardState>::State,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptIrqSaveGuard {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{runtime::task_runtime, test_runtime};

    fn reset_context_state() {
        test_runtime::reset_irq_state();
        test_runtime::reset_preempt_state();
        test_runtime::reset_local_irq_state();
        test_runtime::set_hard_irq(false);
    }

    #[test]
    fn raw_irq_guard_does_not_claim_scheduler_irq_ownership() {
        reset_context_state();

        let guard = IrqSaveGuard::new();
        assert!(!test_runtime::local_irqs_enabled());
        assert_eq!(test_runtime::active_irq_guards(), 0);

        drop(guard);
        assert!(test_runtime::local_irqs_enabled());
    }

    #[test]
    fn preempt_guards_consume_opaque_tokens_in_non_lifo_order() {
        reset_context_state();

        let first = PreemptGuard::new();
        let second = PreemptGuard::new();
        assert_eq!(test_runtime::active_preempt_guards(), 2);

        drop(first);
        assert_eq!(test_runtime::active_preempt_guards(), 1);
        drop(second);
        assert_eq!(test_runtime::active_preempt_guards(), 0);
    }

    #[test]
    fn combined_guard_restores_irq_before_releasing_preemption() {
        reset_context_state();

        let guard = PreemptIrqSaveGuard::new();
        assert!(!test_runtime::local_irqs_enabled());
        assert_eq!(test_runtime::active_preempt_guards(), 1);

        drop(guard);
        assert_eq!(test_runtime::preempt_exit_local_irqs_enabled(), Some(true));
        assert_eq!(test_runtime::active_preempt_guards(), 0);
    }

    #[test]
    fn irq_return_releases_preemption_before_restoring_raw_irq_state() {
        reset_context_state();

        let mut irq = IrqSaveGuard::new();
        let preempt = irq.disable_preempt_for_irq_return();
        assert_eq!(test_runtime::active_preempt_guards(), 1);

        drop(preempt);
        assert_eq!(
            test_runtime::irq_return_exit_local_irqs_enabled(),
            Some(false)
        );
        assert!(!test_runtime::local_irqs_enabled());
        drop(irq);
        assert!(test_runtime::local_irqs_enabled());
    }

    #[test]
    fn hardirq_lifecycle_is_nested_at_the_runtime_owner() {
        reset_context_state();

        hardirq_enter();
        hardirq_enter();
        assert!(task_runtime::in_hard_irq());
        hardirq_exit();
        assert!(task_runtime::in_hard_irq());
        hardirq_exit();
        assert!(!task_runtime::in_hard_irq());
    }
}
