//! FIFO ticket lock coupled to the runtime's nested local IRQ service.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{IrqScope, RawTicketGuard, RawTicketLock};

/// A non-sleeping lock for scheduler state shared with hard-IRQ producers.
///
/// Local IRQ exclusion is acquired before the raw ticket. This mirrors an
/// irqsave runqueue lock: the current CPU cannot deadlock against its own
/// interrupt handler while a remote CPU waits for the same scheduler state.
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

    /// Disables local IRQs and acquires the lock in ticket order.
    pub(crate) fn lock(&self) -> IrqTicketGuard<'_, T> {
        let irq = IrqScope::enter();
        let raw = self.raw.lock();
        IrqTicketGuard {
            raw: Some(raw),
            irq: Some(irq),
            _not_send: PhantomData,
        }
    }

    /// Attempts acquisition and restores local IRQ state on failure.
    #[cfg(test)]
    pub(crate) fn try_lock(&self) -> Option<IrqTicketGuard<'_, T>> {
        let irq = IrqScope::enter();
        self.raw.try_lock().map(|raw| IrqTicketGuard {
            raw: Some(raw),
            irq: Some(irq),
            _not_send: PhantomData,
        })
    }
}

/// IRQ-disabled exclusive access returned by [`IrqTicketLock::lock`].
pub(crate) struct IrqTicketGuard<'a, T> {
    raw: Option<RawTicketGuard<'a, T>>,
    irq: Option<IrqScope>,
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
        // Publish protected state before restoring the IRQ state that can
        // immediately enter a scheduler safe point on this CPU.
        drop(self.raw.take());
        drop(self.irq.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_lock_failure_restores_its_irq_nesting() {
        crate::test_runtime::reset_irq_guard_entries();
        let lock = IrqTicketLock::new(());
        let first = lock.lock();
        assert_eq!(crate::test_runtime::active_irq_guards(), 1);
        assert!(lock.try_lock().is_none());
        assert_eq!(crate::test_runtime::active_irq_guards(), 1);
        drop(first);
        assert_eq!(crate::test_runtime::active_irq_guards(), 0);
    }
}
