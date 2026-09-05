//! Non-sleeping scope coupled to the runtime's nested local IRQ service.

use core::marker::PhantomData;

use crate::runtime::{IrqGuardSource, IrqGuardToken};
#[cfg(not(test))]
use crate::runtime::{enter_irq_guard, task_runtime};

/// Non-sleeping scope that excludes local scheduler preemption.
///
/// Lock-free publications use this across both queue insertion and their
/// matching notification. Otherwise a scheduler IPI can switch out a producer
/// while its stack still owns an epoch-grace registration, leaving the target
/// consumer unable to retire the published head.
pub(crate) struct IrqScope {
    token: IrqGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl IrqScope {
    pub(crate) fn enter() -> Self {
        Self::enter_with_source(IrqGuardSource::ExplicitScope)
    }

    pub(super) fn enter_ticket_lock(source: IrqGuardSource) -> Self {
        Self::enter_with_source(source)
    }

    fn enter_with_source(source: IrqGuardSource) -> Self {
        #[cfg(test)]
        let token = {
            // Crate-local host tests have no kernel IRQ context. Their raw
            // ticket locks still provide cross-thread exclusion, so do not
            // fabricate an OS runtime provider merely to model local IRQs.
            let _ = source;
            IrqGuardToken::NONE
        };
        #[cfg(not(test))]
        let token = enter_irq_guard(source);
        Self {
            token,
            _not_send: PhantomData,
        }
    }
}

impl Drop for IrqScope {
    fn drop(&mut self) {
        if self.token.is_none() {
            return;
        }
        #[cfg(test)]
        unreachable!("unit-test IRQ scopes never own runtime tokens");
        #[cfg(not(test))]
        // SAFETY: construction received this token on the current CPU, the
        // !Send marker prevents migration, and Drop consumes it exactly once.
        unsafe {
            task_runtime::irq_guard_exit(self.token)
        };
    }
}
