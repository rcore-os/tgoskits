use crate::{arch::register::irq::TI, irq::IrqId};

pub fn systimer_irq() -> IrqId {
    IrqId::new(TI as usize)
}
