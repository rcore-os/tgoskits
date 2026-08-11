//! Execution-context guards used by synchronization adapters.

use core::marker::PhantomData;

use crate::interface::{CONTEXT_IRQSAVE, CONTEXT_PREEMPT, CONTEXT_PREEMPT_IRQSAVE, ContextState};

/// Saves the local interrupt state and disables local interrupts.
#[doc(hidden)]
#[inline(always)]
pub fn irq_save_and_disable() -> usize {
    crate::interface::context_enter(CONTEXT_IRQSAVE).irq()
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
    crate::interface::context_exit(CONTEXT_IRQSAVE, ContextState::new(0, state));
}

/// Internal critical-section contract retained for low-level adapters.
#[doc(hidden)]
pub trait GuardState {
    /// Saved state needed when the guard is released.
    type State: Clone + Copy;

    /// Enters the critical section.
    fn acquire() -> Self::State;

    /// Leaves the critical section.
    fn release(state: Self::State);
}

/// Raw state which does not alter the execution context.
#[doc(hidden)]
pub struct RawState;

/// State which disables kernel preemption.
#[doc(hidden)]
pub struct PreemptState;

/// State which saves and disables local interrupts.
#[doc(hidden)]
pub struct IrqSaveState;

/// State which disables preemption, then saves and disables interrupts.
#[doc(hidden)]
pub struct PreemptIrqSaveState;

impl GuardState for RawState {
    type State = ();

    fn acquire() -> Self::State {}

    fn release(_state: Self::State) {}
}

impl GuardState for PreemptState {
    type State = ContextState;

    fn acquire() -> Self::State {
        crate::interface::context_enter(CONTEXT_PREEMPT)
    }

    fn release(state: Self::State) {
        crate::interface::context_exit(CONTEXT_PREEMPT, state);
    }
}

impl GuardState for IrqSaveState {
    type State = ContextState;

    fn acquire() -> Self::State {
        crate::interface::context_enter(CONTEXT_IRQSAVE)
    }

    fn release(state: Self::State) {
        crate::interface::context_exit(CONTEXT_IRQSAVE, state);
    }
}

impl GuardState for PreemptIrqSaveState {
    type State = ContextState;

    fn acquire() -> Self::State {
        crate::interface::context_enter(CONTEXT_PREEMPT_IRQSAVE)
    }

    fn release(state: Self::State) {
        crate::interface::context_exit(CONTEXT_PREEMPT_IRQSAVE, state);
    }
}

/// An RAII guard which disables kernel preemption while it is alive.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::PreemptGuard>();
/// ```
pub struct PreemptGuard {
    state: ContextState,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptGuard {
    /// Disables preemption until the returned guard is dropped.
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

/// An RAII guard which saves and disables local interrupts.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::IrqSaveGuard>();
/// ```
pub struct IrqSaveGuard {
    state: ContextState,
    _not_send: PhantomData<*mut ()>,
}

impl IrqSaveGuard {
    /// Saves and disables local interrupts until drop.
    pub fn new() -> Self {
        Self {
            state: IrqSaveState::acquire(),
            _not_send: PhantomData,
        }
    }

    /// Disables preemption for work completed by a hard-IRQ return epilogue.
    pub fn disable_preempt_for_irq_return(&mut self) -> IrqReturnPreemptGuard<'_> {
        IrqReturnPreemptGuard {
            state: crate::interface::irq_return_preempt_enter(),
            _irq_guard: PhantomData,
            _not_send: PhantomData,
        }
    }
}

/// A preemption guard whose final release is an IRQ-return boundary.
#[must_use = "dropping the guard completes IRQ-return preemption exit"]
pub struct IrqReturnPreemptGuard<'irq> {
    state: usize,
    _irq_guard: PhantomData<&'irq mut IrqSaveGuard>,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for IrqReturnPreemptGuard<'_> {
    fn drop(&mut self) {
        crate::interface::irq_return_preempt_exit(self.state);
    }
}

/// Publishes entry into the runtime hard-interrupt lifecycle.
pub fn hardirq_enter() {
    crate::interface::hardirq_enter();
}

/// Publishes exit from the runtime hard-interrupt lifecycle.
pub fn hardirq_exit() {
    crate::interface::hardirq_exit();
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
/// before re-enabling preemption.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_sync::PreemptIrqSaveGuard>();
/// ```
pub struct PreemptIrqSaveGuard {
    state: ContextState,
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
