//! Non-sleeping scope coupled to the runtime's nested local IRQ service.

use core::marker::PhantomData;

use crate::runtime::{IrqGuardToken, task_runtime};

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
        Self {
            token: task_runtime::irq_guard_enter(),
            _not_send: PhantomData,
        }
    }
}

impl Drop for IrqScope {
    fn drop(&mut self) {
        // SAFETY: construction received this token on the current CPU, the
        // !Send marker prevents migration, and Drop consumes it exactly once.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}
