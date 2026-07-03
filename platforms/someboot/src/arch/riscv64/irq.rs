use crate::irq::IrqId;

pub fn systimer_irq() -> IrqId {
    IrqId::new(5)
}
