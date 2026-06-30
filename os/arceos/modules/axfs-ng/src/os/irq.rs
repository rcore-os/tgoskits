use alloc::{boxed::Box, string::String};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::AxResult;
use ax_kspin::SpinRwLock as RwLock;
use irq_framework::{IrqContext, IrqId};

use crate::block::runtime::BlockIrqAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockIrqOutcome {
    Handled,
    Wake,
}

pub trait BlockIrqRegistration: Send + Sync {}

pub trait BlockIrqRegistrar: Send + Sync {
    fn register_shared(
        &self,
        name: String,
        irq: IrqId,
        action: Box<dyn FnMut(IrqContext) -> BlockIrqOutcome + Send + 'static>,
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
    irq: IrqId,
    action: BlockIrqAction,
) -> AxResult<Box<dyn BlockIrqRegistration>> {
    let registrar = IRQ_REGISTRAR
        .read()
        .as_ref()
        .copied()
        .ok_or(ax_errno::AxError::BadState)?;
    let mut action = action;
    registrar.register_shared(name, irq, Box::new(move |_ctx| action.run()))
}

pub fn has_irq_registrar() -> bool {
    IRQ_READY.load(Ordering::Acquire)
}
