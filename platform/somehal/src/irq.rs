use rdif_intc::Intc;
pub use rdif_intc::{self, IrqId};
use rdrive::DeviceId;
pub use someboot::irq::*;

use crate::{arch::Plat, common::PlatOp};

pub fn irq_setup_by_fdt(irq_parent: DeviceId, irq_cell: &[u32]) -> IrqId {
    let mut intc = rdrive::get::<Intc>(irq_parent).unwrap().lock().unwrap();
    debug!("Setting up IRQ {:?}", irq_cell);
    let id: usize = intc.setup_irq_by_fdt(irq_cell).into();
    id.into()
}

pub fn irq_set_enable(irq: IrqId, enable: bool) {
    debug!("Setting IRQ {:?} enable to {}", irq, enable);
    Plat::irq_set_enable(irq, enable);
}

pub fn systick_irq() -> IrqId {
    Plat::systick_irq()
}

pub(crate) fn _handle_irq(hwirq: IrqId) {
    unsafe extern "Rust" {
        fn _someboot_handle_irq(hwirq: IrqId);
    }
    unsafe {
        _someboot_handle_irq(hwirq);
    }
}

pub fn irq_handler_raw() -> IrqId {
    Plat::irq_handler().raw().into()
}
