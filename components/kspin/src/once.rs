//! Preemption-aware one-time initialization primitives.

use core::ops::Deref;

use spin::Once;

use crate::PreemptGuard;

/// A one-time initializer whose owner cannot be switched out on its CPU.
///
/// A raw spin-based [`Once`] may publish its `Running` state and then be
/// preempted. If the replacement task reaches the same initializer while
/// holding an IRQ or preemption guard, it spins forever because the owner can
/// no longer run. `PreemptOnce` closes that inversion window by retaining a
/// [`PreemptGuard`] from before the initialization race until the value is
/// published.
///
/// Initializers must run in ordinary task or deferred context. They may
/// allocate, but must not block, sleep, or recursively initialize the same
/// object. Completed reads do not enter a runtime context guard.
pub struct PreemptOnce<T> {
    inner: Once<T>,
}

impl<T> PreemptOnce<T> {
    /// Creates an uninitialized value.
    pub const fn new() -> Self {
        Self { inner: Once::new() }
    }

    /// Returns the initialized value without waiting for an in-flight owner.
    pub fn get(&self) -> Option<&T> {
        self.inner.get()
    }

    /// Returns mutable access when the caller uniquely owns the initializer.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.inner.get_mut()
    }

    /// Gets the value or initializes it while retaining same-CPU ownership.
    pub fn call_once(&self, initializer: impl FnOnce() -> T) -> &T {
        if let Some(value) = self.inner.get() {
            return value;
        }

        let _preempt = PreemptGuard::new();
        self.inner.call_once(initializer)
    }
}

impl<T> Default for PreemptOnce<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Lazily initializes one value under the [`PreemptOnce`] ownership contract.
///
/// This compact form accepts a non-capturing function pointer so it can be
/// constructed in static storage without another mutable initializer slot.
pub struct PreemptLazy<T> {
    value: PreemptOnce<T>,
    initializer: fn() -> T,
}

impl<T> PreemptLazy<T> {
    /// Creates a lazy value from a non-capturing initializer.
    pub const fn new(initializer: fn() -> T) -> Self {
        Self {
            value: PreemptOnce::new(),
            initializer,
        }
    }

    /// Forces initialization and returns the published value.
    pub fn force(this: &Self) -> &T {
        this.value.call_once(this.initializer)
    }

    /// Returns the value only when initialization has completed.
    pub fn get(&self) -> Option<&T> {
        self.value.get()
    }
}

impl<T> Deref for PreemptLazy<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        Self::force(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_call::imp;

    #[test]
    fn initializer_retains_preemption_without_masking_irqs() {
        imp::reset();
        let once = PreemptOnce::new();

        let value = once.call_once(|| {
            let (irq_depth, preempt_depth, scheduled, events) = imp::snapshot();
            assert_eq!((irq_depth, preempt_depth, scheduled), (0, 1, 0));
            assert_eq!(events, ["preempt-enter"]);
            42
        });

        assert_eq!(*value, 42);
        let (irq_depth, preempt_depth, scheduled, events) = imp::snapshot();
        assert_eq!((irq_depth, preempt_depth, scheduled), (0, 0, 0));
        assert_eq!(events, ["preempt-enter", "preempt-exit"]);
    }

    #[test]
    fn completed_fast_path_does_not_enter_a_context_guard() {
        let once = PreemptOnce::new();
        imp::reset();
        assert_eq!(*once.call_once(|| 7), 7);

        imp::reset();
        assert_eq!(*once.call_once(|| 9), 7);
        assert_eq!(imp::snapshot(), (0, 0, 0, std::vec![]));
    }
}
