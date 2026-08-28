use alloc::{boxed::Box, string::String};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_sync::SpinRwLock as RwLock;
use irq_framework::IrqId;

use crate::{BlockError, BlockResult, block::runtime::BlockIrqAction};

/// Result returned from the runtime-independent hard IRQ action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockIrqOutcome {
    /// The device did not assert this shared interrupt.
    Unhandled,
    /// The device source was acknowledged without publishing deferred work.
    Handled,
    /// The source was acknowledged and a maintenance task was activated.
    Wake,
}

/// Owned IRQ registration and boxed hard-handler lifetime token.
pub trait BlockIrqRegistration: Send + Sync {
    /// Enables the registered action after all runtime state is published.
    fn enable(&self) -> BlockResult;

    /// Disables the action and waits for every in-flight callback to return.
    fn disable_and_synchronize(&self) -> BlockResult;
}

/// Registers fixed-affinity non-reentrant block hard IRQ actions.
pub trait BlockIrqRegistrar: Send + Sync {
    /// Registers an action disabled on the requested CPU.
    ///
    /// # Errors
    ///
    /// Returns an error when the IRQ cannot be registered with
    /// `NonReentrant`, `AutoEnable::No`, and fixed affinity.
    fn register(
        &self,
        name: String,
        irq: IrqId,
        cpu: usize,
        action: BlockIrqAction,
    ) -> BlockResult<Box<dyn BlockIrqRegistration>>;
}

static IRQ_REGISTRAR: RwLock<Option<&'static dyn BlockIrqRegistrar>> = RwLock::new(None);
static IRQ_READY: AtomicBool = AtomicBool::new(false);

/// Installs the runtime IRQ registrar.
pub fn set_irq_registrar(registrar: &'static dyn BlockIrqRegistrar) {
    *IRQ_REGISTRAR.write() = Some(registrar);
    IRQ_READY.store(true, Ordering::Release);
}

/// Registers one fixed-affinity block IRQ action.
///
/// # Errors
///
/// Returns [`BlockError::RuntimeUnavailable`] before the runtime installs an IRQ registrar,
/// or propagates registration failures.
pub fn register_block_irq(
    name: String,
    irq: IrqId,
    cpu: usize,
    action: BlockIrqAction,
) -> BlockResult<Box<dyn BlockIrqRegistration>> {
    IRQ_REGISTRAR
        .read()
        .as_ref()
        .copied()
        .ok_or(BlockError::RuntimeUnavailable)?
        .register(name, irq, cpu, action)
}

/// Returns whether an IRQ registrar is installed.
pub fn has_irq_registrar() -> bool {
    irq_registrar_ready(&IRQ_READY)
}

fn irq_registrar_ready(ready: &AtomicBool) -> bool {
    ready.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_irq_outcomes_keep_handled_and_wake_distinct() {
        let handled = BlockIrqOutcome::Handled;
        let copied = handled;

        assert_eq!(copied, BlockIrqOutcome::Handled);
        assert_ne!(handled, BlockIrqOutcome::Wake);
    }

    #[test]
    fn irq_registrar_readiness_starts_unpublished() {
        let ready = AtomicBool::new(false);

        assert!(!irq_registrar_ready(&ready));
        ready.store(true, Ordering::Release);
        assert!(irq_registrar_ready(&ready));
    }
}
