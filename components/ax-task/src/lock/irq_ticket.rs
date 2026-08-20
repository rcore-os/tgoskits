//! FIFO ticket lock coupled to the runtime's nested local IRQ service.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{IrqScope, RawTicketGuard, RawTicketLock};
use crate::runtime::IrqGuardSource;

/// A non-sleeping lock for scheduler state shared with hard-IRQ producers.
///
/// Local IRQ exclusion is acquired before the raw ticket. This mirrors an
/// irqsave runqueue lock: the current CPU cannot deadlock against its own
/// interrupt handler while a remote CPU waits for the same scheduler state.
#[derive(Debug)]
pub(crate) struct IrqTicketLock<T> {
    raw: RawTicketLock<T>,
}

/// Borrowed proof that one outer guard retains local IRQ exclusion.
///
/// The lifetime is tied to the mutable borrow of that guard. Nested raw ticket
/// guards therefore cannot outlive the runtime IRQ owner that protects them.
pub(crate) struct IrqOwner<'a> {
    _guard: PhantomData<&'a mut ()>,
    _not_send: PhantomData<*mut ()>,
}

impl<T> IrqTicketLock<T> {
    /// Creates an unlocked IRQ-safe ticket lock.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            raw: RawTicketLock::new(value),
        }
    }

    /// Disables local IRQs and acquires the lock in ticket order.
    pub(crate) fn lock(&self, source: IrqGuardSource) -> IrqTicketGuard<'_, T> {
        let irq = IrqScope::enter_ticket_lock(source);
        let raw = self.raw.lock();
        IrqTicketGuard {
            raw: Some(raw),
            irq: Some(irq),
            _not_send: PhantomData,
        }
    }

    /// Locks scheduler state when the caller already owns an IRQ-off CPU.
    ///
    /// # Safety
    ///
    /// Local IRQs must remain disabled until the returned guard is dropped.
    /// The caller must own either the architecture scheduler baton or the
    /// offline boot CPU's non-preemptible initialization context. Ordinary task
    /// context must use [`Self::lock`], otherwise a local hard IRQ could
    /// deadlock on the same raw ticket lock.
    pub(crate) unsafe fn lock_irq_disabled(&self) -> IrqTicketGuard<'_, T> {
        IrqTicketGuard {
            raw: Some(self.raw.lock()),
            irq: None,
            _not_send: PhantomData,
        }
    }

    /// Acquires a nested ticket under a borrowed outer IRQ owner.
    ///
    /// This is the typed equivalent of Linux taking `p->pi_lock` with
    /// `irqsave` and then taking `rq->lock` raw. The returned guard borrows the
    /// owner proof, so restoring the outer IRQ state first is not expressible.
    pub(crate) fn lock_nested<'a>(&'a self, _owner: &'a IrqOwner<'_>) -> IrqTicketGuard<'a, T> {
        IrqTicketGuard {
            raw: Some(self.raw.lock()),
            irq: None,
            _not_send: PhantomData,
        }
    }

    /// Attempts acquisition and restores local IRQ state on failure.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn try_lock(&self, source: IrqGuardSource) -> Option<IrqTicketGuard<'_, T>> {
        let irq = IrqScope::enter_ticket_lock(source);
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

impl<T> IrqTicketGuard<'_, T> {
    /// Splits protected state from a proof that this guard retains IRQ-off.
    pub(crate) fn split_irq_owner(&mut self) -> (&mut T, IrqOwner<'_>) {
        let state = self
            .raw
            .as_deref_mut()
            .expect("IRQ ticket guard always owns its raw guard");
        (
            state,
            IrqOwner {
                _guard: PhantomData,
                _not_send: PhantomData,
            },
        )
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) const fn owns_runtime_irq_scope(&self) -> bool {
        self.irq.is_some()
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
