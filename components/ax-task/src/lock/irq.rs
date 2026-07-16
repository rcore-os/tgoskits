//! Ticket lock coupled to the runtime's nested local IRQ service.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{RawTicketGuard, RawTicketLock};
use crate::runtime::{IrqGuardToken, task_runtime};

/// A private ticket lock that disables local IRQs before waiting.
#[derive(Debug)]
pub(crate) struct IrqTicketLock<T> {
    raw: RawTicketLock<T>,
}

impl<T> IrqTicketLock<T> {
    /// Creates an unlocked IRQ-safe ticket lock.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            raw: RawTicketLock::new(value),
        }
    }

    /// Disables local IRQs through the nested runtime service and acquires.
    pub(crate) fn lock(&self) -> IrqTicketGuard<'_, T> {
        let token = task_runtime::irq_guard_enter();
        let raw = self.raw.lock();
        IrqTicketGuard {
            raw: Some(raw),
            token,
            _not_send: PhantomData,
        }
    }

    /// Attempts acquisition and restores the entered IRQ context on failure.
    pub(crate) fn try_lock(&self) -> Option<IrqTicketGuard<'_, T>> {
        let token = task_runtime::irq_guard_enter();
        match self.raw.try_lock() {
            Some(raw) => Some(IrqTicketGuard {
                raw: Some(raw),
                token,
                _not_send: PhantomData,
            }),
            None => {
                // SAFETY: this call consumes the token just returned above on
                // the same CPU; no lock guard escaped the failed acquisition.
                unsafe { task_runtime::irq_guard_exit(token) };
                None
            }
        }
    }
}

/// IRQ-disabled access to an internal scheduler object.
pub(crate) struct IrqTicketGuard<'a, T> {
    raw: Option<RawTicketGuard<'a, T>>,
    token: IrqGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl<T> Deref for IrqTicketGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.raw
            .as_deref()
            .expect("IRQ ticket guard always owns its raw guard")
    }
}

impl<T> DerefMut for IrqTicketGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.raw
            .as_deref_mut()
            .expect("IRQ ticket guard always owns its raw guard")
    }
}

impl<T> Drop for IrqTicketGuard<'_, T> {
    fn drop(&mut self) {
        // Release publication must precede IRQ restoration. A restored IRQ may
        // immediately attempt this lock or enter the scheduler.
        drop(self.raw.take());
        // SAFETY: guard construction received this token on the current CPU and
        // this Drop consumes it exactly once. The runtime accepts non-LIFO exit.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_lock_failure_restores_its_irq_nesting() {
        crate::test_runtime::reset_irq_state();
        let lock = IrqTicketLock::new(());
        let first = lock.lock();
        assert_eq!(crate::test_runtime::active_irq_guards(), 1);
        assert!(lock.try_lock().is_none());
        assert_eq!(crate::test_runtime::active_irq_guards(), 1);
        drop(first);
        assert_eq!(crate::test_runtime::active_irq_guards(), 0);
    }

    #[test]
    fn non_lifo_guard_drop_keeps_irqs_disabled_until_the_last_guard() {
        crate::test_runtime::reset_irq_state();
        let first = IrqTicketLock::new(());
        let second = IrqTicketLock::new(());
        let first_guard = first.lock();
        let second_guard = second.lock();
        drop(first_guard);
        assert_eq!(crate::test_runtime::active_irq_guards(), 1);
        drop(second_guard);
        assert_eq!(crate::test_runtime::active_irq_guards(), 0);
    }
}
