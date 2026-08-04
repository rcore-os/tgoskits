use alloc::{boxed::Box, string::String};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinRwLock as RwLock;
use irq_framework::IrqId;

use crate::block::runtime::BlockIrqAction;

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
    fn enable(&self) -> AxResult;

    /// Disables the action and waits for every in-flight callback to return.
    fn disable_and_synchronize(&self) -> AxResult;
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
    ) -> AxResult<Box<dyn BlockIrqRegistration>>;
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
/// Returns [`AxError::BadState`] before the runtime installs an IRQ registrar,
/// or propagates registration failures.
pub fn register_block_irq(
    name: String,
    irq: IrqId,
    cpu: usize,
    action: BlockIrqAction,
) -> AxResult<Box<dyn BlockIrqRegistration>> {
    IRQ_REGISTRAR
        .read()
        .as_ref()
        .copied()
        .ok_or(AxError::BadState)?
        .register(name, irq, cpu, action)
}

/// Returns whether an IRQ registrar is installed.
pub fn has_irq_registrar() -> bool {
    IRQ_READY.load(Ordering::Acquire)
}

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) fn block_irq_outcome_and_ready_hold_for_test() -> bool {
    // Test BlockIrqOutcome variants
    let handled = BlockIrqOutcome::Handled;
    let wake = BlockIrqOutcome::Wake;

    assert!(handled != wake);

    // Test Clone, Copy, Debug, Eq, PartialEq
    let _cloned = handled;

    // Test has_irq_registrar returns false initially (no registrar set)
    assert!(!has_irq_registrar());

    true
}
