//! Context policies and standalone context guards.

use core::mem::ManuallyDrop;

use ax_cpu_local::CpuPin;

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
    cpu_pin: CpuPin,
}

/// A preemption guard backed by the runtime's nested preemption service.
#[must_use = "dropping the guard leaves the preemption-disabled section"]
pub struct PreemptGuard {
    cpu_pin: CpuPin,
}

/// A combined preemption and local-IRQ guard.
#[must_use = "dropping the guard restores the runtime context"]
pub struct PreemptIrqGuard {
    cpu_pin: CpuPin,
}

impl IrqGuard {
    /// Enters one nested local-IRQ-disabled section.
    #[inline(always)]
    pub fn new() -> Self {
        IrqSaveContext::enter();
        Self {
            // SAFETY: LockRuntime's IRQ nesting is a scheduler barrier until
            // this same-CPU guard leaves the IRQ-disabled section.
            cpu_pin: unsafe { CpuPin::new_unchecked() },
        }
    }

    /// Borrows proof that CPU-local address calculation cannot race migration.
    #[inline(always)]
    pub const fn cpu_pin(&self) -> &CpuPin {
        &self.cpu_pin
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
            // SAFETY: LockRuntime keeps the calling execution context pinned
            // until this guard performs its matching preemption exit.
            cpu_pin: unsafe { CpuPin::new_unchecked() },
        }
    }

    /// Borrows proof that CPU-local address calculation cannot race migration.
    #[inline(always)]
    pub const fn cpu_pin(&self) -> &CpuPin {
        &self.cpu_pin
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
        // SAFETY: forwarded caller contract is exactly the LockRuntime IRQ-return
        // contract. The runtime owns the atomic eligibility recheck and schedule.
        unsafe { runtime_call::preempt_exit_irq_return() };
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
            // SAFETY: both preemption and local IRQ nesting remain active for
            // this guard's complete lifetime.
            cpu_pin: unsafe { CpuPin::new_unchecked() },
        }
    }

    /// Borrows proof that CPU-local address calculation cannot race migration.
    #[inline(always)]
    pub const fn cpu_pin(&self) -> &CpuPin {
        &self.cpu_pin
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
    runtime_call::preempt_exit();
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
                "schedule",
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
                "preempt-exit-irq-return",
                "schedule-irq-return",
            ]
        );
        imp::irq_exit();
    }
}
