//! Context policies and standalone context guards.

use core::{marker::PhantomData, mem::ManuallyDrop};

use crate::runtime_call;

/// A sealed context transition used by raw lock implementations.
pub trait LockContext: private::Sealed + Send + Sync + 'static {
    /// Enters the context required before acquiring the raw lock.
    fn enter();

    /// Leaves the context after the raw lock has been released.
    fn exit();
}

/// Context marker for locks that do not alter IRQ or preemption state.
#[derive(Clone, Copy, Debug, Default)]
pub struct RawContext;

/// Context marker for locks that disable preemption.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPreemptContext;

/// Context marker for locks that save and disable local interrupts.
#[derive(Clone, Copy, Debug, Default)]
pub struct IrqSaveContext;

/// Context marker for locks that disable preemption and local interrupts.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPreemptIrqSaveContext;

/// A local-IRQ guard backed by the runtime's nested IRQ service.
#[must_use = "dropping the guard leaves the IRQ-disabled section"]
pub struct IrqGuard {
    _not_send: PhantomData<*mut ()>,
}

/// A preemption guard backed by the runtime's nested preemption service.
#[must_use = "dropping the guard leaves the preemption-disabled section"]
pub struct PreemptGuard {
    _not_send: PhantomData<*mut ()>,
}

/// A combined preemption and local-IRQ guard.
#[must_use = "dropping the guard restores the runtime context"]
pub struct PreemptIrqGuard {
    _not_send: PhantomData<*mut ()>,
}

impl IrqGuard {
    /// Enters one nested local-IRQ-disabled section.
    #[inline(always)]
    pub fn new() -> Self {
        IrqSaveContext::enter();
        Self {
            _not_send: PhantomData,
        }
    }
}

impl Default for IrqGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrqGuard {
    #[inline(always)]
    fn drop(&mut self) {
        IrqSaveContext::exit();
    }
}

impl PreemptGuard {
    /// Enters one nested preemption-disabled section.
    #[inline(always)]
    pub fn new() -> Self {
        NoPreemptContext::enter();
        Self {
            _not_send: PhantomData,
        }
    }

    /// Leaves the guard at the architecture IRQ-return scheduler safe point.
    ///
    /// Unlike ordinary guard drop, this may schedule while hardware IRQs are
    /// still disabled. The architecture trap frame, rather than this guard,
    /// remains responsible for restoring the interrupted IRQ state.
    ///
    /// # Safety
    ///
    /// The interrupt controller must have completed EOI, the runtime's hard-IRQ
    /// marker must be clear, and the caller must be returning through a trap
    /// frame that preserves the interrupted IRQ flags.
    #[inline(always)]
    pub unsafe fn finish_irq_return(self) {
        let _guard = ManuallyDrop::new(self);
        leave_preempt_context_inner(true);
    }
}

impl Default for PreemptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptGuard {
    #[inline(always)]
    fn drop(&mut self) {
        NoPreemptContext::exit();
    }
}

impl PreemptIrqGuard {
    /// Disables preemption before entering a nested IRQ-disabled section.
    #[inline(always)]
    pub fn new() -> Self {
        NoPreemptIrqSaveContext::enter();
        Self {
            _not_send: PhantomData,
        }
    }
}

impl Default for PreemptIrqGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptIrqGuard {
    #[inline(always)]
    fn drop(&mut self) {
        NoPreemptIrqSaveContext::exit();
    }
}

impl LockContext for RawContext {
    #[inline(always)]
    fn enter() {}

    #[inline(always)]
    fn exit() {}
}

impl LockContext for NoPreemptContext {
    #[inline(always)]
    fn enter() {
        runtime_call::preempt_enter();
    }

    #[inline(always)]
    fn exit() {
        leave_preempt_context();
    }
}

impl LockContext for IrqSaveContext {
    #[inline(always)]
    fn enter() {
        runtime_call::irq_enter();
    }

    #[inline(always)]
    fn exit() {
        runtime_call::irq_exit();
    }
}

impl LockContext for NoPreemptIrqSaveContext {
    #[inline(always)]
    fn enter() {
        runtime_call::preempt_enter();
        runtime_call::irq_enter();
    }

    #[inline(always)]
    fn exit() {
        runtime_call::irq_exit();
        leave_preempt_context();
    }
}

#[inline(always)]
fn leave_preempt_context() {
    leave_preempt_context_inner(false);
}

#[inline(always)]
fn leave_preempt_context_inner(irq_return: bool) {
    let outermost = runtime_call::preempt_exit();
    if outermost && should_schedule(irq_return) {
        // Keep this task non-preemptible across the context switch. The runtime
        // saves this depth in the outgoing execution context and restores the
        // incoming task's own depth, so another task never inherits it. When
        // this task resumes, the retained depth prevents a second scheduling
        // frame from nesting before the first one has unwound.
        runtime_call::preempt_enter();
        runtime_call::schedule();
        debug_assert!(
            runtime_call::preempt_exit(),
            "scheduler changed the caller's preemption nesting"
        );
    }
}

#[inline(always)]
fn should_schedule(irq_return: bool) -> bool {
    (irq_return || runtime_call::irqs_enabled())
        && !runtime_call::in_hard_irq()
        && runtime_call::need_resched()
}

mod private {
    pub trait Sealed {}

    impl Sealed for super::RawContext {}
    impl Sealed for super::NoPreemptContext {}
    impl Sealed for super::IrqSaveContext {}
    impl Sealed for super::NoPreemptIrqSaveContext {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_call::imp;

    #[test]
    fn non_lifo_irq_guards_do_not_restore_irqs_early() {
        imp::reset();
        let first = IrqGuard::new();
        let second = IrqGuard::new();

        drop(first);
        assert_eq!(imp::snapshot().0, 1);

        drop(second);
        assert_eq!(imp::snapshot().0, 0);
    }

    #[test]
    fn combined_exit_restores_irqs_and_schedules_at_most_once() {
        imp::reset();
        imp::set_need_resched();
        let guard = PreemptIrqGuard::new();

        drop(guard);

        let (irq_depth, preempt_depth, scheduled, events) = imp::snapshot();
        assert_eq!((irq_depth, preempt_depth, scheduled), (0, 0, 1));
        assert_eq!(
            events,
            [
                "preempt-enter",
                "irq-enter",
                "irq-exit",
                "preempt-exit",
                "preempt-enter",
                "schedule",
                "preempt-exit",
            ]
        );
    }

    #[test]
    fn irq_return_schedules_once_without_enabling_irqs() {
        imp::reset();
        imp::set_need_resched();
        imp::irq_enter();
        let guard = PreemptGuard::new();

        unsafe {
            // SAFETY: the fake IRQ marker is clear and the explicit fake IRQ
            // nesting models a trap frame that will restore its own state.
            guard.finish_irq_return();
        }

        let (irq_depth, preempt_depth, scheduled, events) = imp::snapshot();
        assert_eq!((irq_depth, preempt_depth, scheduled), (1, 0, 1));
        assert_eq!(
            events,
            [
                "irq-enter",
                "preempt-enter",
                "preempt-exit",
                "preempt-enter",
                "schedule",
                "preempt-exit",
            ]
        );
        imp::irq_exit();
    }
}
