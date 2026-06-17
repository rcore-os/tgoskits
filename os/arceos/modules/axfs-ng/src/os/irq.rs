use alloc::{boxed::Box, string::String};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::AxResult;
use spin::RwLock;

use crate::block::runtime::BlockIrqAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockIrqOutcome {
    Handled,
}

pub trait BlockIrqRegistration: Send + Sync {}

pub trait BlockIrqRegistrar: Send + Sync {
    fn register_shared(
        &self,
        name: String,
        irq: usize,
        action: BlockIrqAction,
    ) -> AxResult<Box<dyn BlockIrqRegistration>>;
}

static IRQ_REGISTRAR: RwLock<Option<&'static dyn BlockIrqRegistrar>> = RwLock::new(None);
static IRQ_READY: AtomicBool = AtomicBool::new(false);

pub fn set_irq_registrar(registrar: &'static dyn BlockIrqRegistrar) {
    *IRQ_REGISTRAR.write() = Some(registrar);
    IRQ_READY.store(true, Ordering::Release);
}

pub fn register_shared_block_irq(
    name: String,
    irq: usize,
    action: BlockIrqAction,
) -> AxResult<Box<dyn BlockIrqRegistration>> {
    IRQ_REGISTRAR
        .read()
        .as_ref()
        .ok_or(ax_errno::AxError::BadState)?
        .register_shared(name, irq, action)
}

pub fn has_irq_registrar() -> bool {
    IRQ_READY.load(Ordering::Acquire)
}
