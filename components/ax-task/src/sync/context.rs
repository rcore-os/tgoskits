//! Task-owned execution-context guards for synchronization primitives.

use core::marker::PhantomData;

use crate::runtime::{
    LocalIrqState, PreemptGuardSource, PreemptGuardToken, enter_preempt_guard, task_runtime,
};

pub(crate) trait ContextBackend {
    type PreemptState: Copy;
    type IrqState: Copy;

    fn preempt_enter(&self) -> Self::PreemptState;
    fn preempt_exit(&self, state: Self::PreemptState);
    fn preempt_exit_irq_return(&self, state: Self::PreemptState);
    fn irq_save_and_disable(&self) -> Self::IrqState;
    fn irq_restore(&self, state: Self::IrqState);
}

pub(crate) struct TaskRuntimeContext;

impl ContextBackend for TaskRuntimeContext {
    type PreemptState = PreemptGuardToken;
    type IrqState = LocalIrqState;

    fn preempt_enter(&self) -> Self::PreemptState {
        enter_preempt_guard(PreemptGuardSource::SyncContext)
    }

    fn preempt_exit(&self, state: Self::PreemptState) {
        if state.is_none() {
            return;
        }
        // SAFETY: the matching acquire returned this same-context token.
        unsafe { task_runtime::preempt_guard_exit(state) };
    }

    fn preempt_exit_irq_return(&self, state: Self::PreemptState) {
        if state.is_none() {
            return;
        }
        // SAFETY: the matching acquire returned this token, and IRQ-return
        // consumes it while the raw local-IRQ guard remains active.
        unsafe { task_runtime::preempt_guard_exit_irq_return(state) };
    }

    fn irq_save_and_disable(&self) -> Self::IrqState {
        task_runtime::local_irq_save_and_disable()
    }

    fn irq_restore(&self, state: Self::IrqState) {
        // SAFETY: the matching acquire returned this raw state on this CPU.
        unsafe { task_runtime::local_irq_restore(state) };
    }
}

struct PendingPreempt<'backend, B: ContextBackend> {
    backend: &'backend B,
    state: Option<B::PreemptState>,
}

impl<'backend, B: ContextBackend> PendingPreempt<'backend, B> {
    fn acquire(backend: &'backend B) -> Self {
        Self {
            backend,
            state: Some(backend.preempt_enter()),
        }
    }

    fn into_state(mut self) -> B::PreemptState {
        self.state
            .take()
            .expect("pending preemption state must be owned")
    }
}

impl<B: ContextBackend> Drop for PendingPreempt<'_, B> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            self.backend.preempt_exit(state);
        }
    }
}

pub(crate) fn enter_preempt_irqsave<B: ContextBackend>(
    backend: &B,
) -> (B::PreemptState, B::IrqState) {
    let preempt = PendingPreempt::acquire(backend);
    let irq = backend.irq_save_and_disable();
    (preempt.into_state(), irq)
}

pub(crate) fn exit_preempt_irqsave<B: ContextBackend>(
    (preempt, irq): (B::PreemptState, B::IrqState),
    backend: &B,
) {
    backend.irq_restore(irq);
    backend.preempt_exit(preempt);
}

/// Internal critical-section contract shared by task-owned lock algorithms.
pub trait GuardState {
    type State: Copy;

    fn acquire() -> Self::State;

    fn release(state: Self::State);

    #[cfg(feature = "lockdep")]
    fn lockdep_enabled() -> bool {
        false
    }
}

pub struct RawState;
pub struct PreemptState;
pub struct IrqSaveState;
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
        TaskRuntimeContext.preempt_enter()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        TaskRuntimeContext.preempt_exit(state);
    }

    #[cfg(feature = "lockdep")]
    fn lockdep_enabled() -> bool {
        true
    }
}

impl GuardState for IrqSaveState {
    type State = LocalIrqState;

    #[inline(always)]
    fn acquire() -> Self::State {
        TaskRuntimeContext.irq_save_and_disable()
    }

    #[inline(always)]
    fn release(state: Self::State) {
        TaskRuntimeContext.irq_restore(state);
    }
}

impl GuardState for PreemptIrqSaveState {
    type State = (PreemptGuardToken, LocalIrqState);

    #[inline(always)]
    fn acquire() -> Self::State {
        enter_preempt_irqsave(&TaskRuntimeContext)
    }

    #[inline(always)]
    fn release((preempt, irq): Self::State) {
        exit_preempt_irqsave((preempt, irq), &TaskRuntimeContext);
    }

    #[cfg(feature = "lockdep")]
    fn lockdep_enabled() -> bool {
        true
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
            token: enter_preempt_guard(PreemptGuardSource::IrqReturn),
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
        TaskRuntimeContext.preempt_exit_irq_return(self.token);
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
