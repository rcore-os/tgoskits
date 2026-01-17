use crate::common::PlatOp;

pub mod gic;
pub mod systick;

pub struct Plat;

impl PlatOp for Plat {
    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        gic::irq_set_enable(irq, enable);
    }

    fn systick_irq() -> rdrive::IrqId {
        systick::systick_irq()
    }
}
