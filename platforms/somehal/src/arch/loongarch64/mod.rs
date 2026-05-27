use crate::common::PlatOp;

pub struct Plat;

impl PlatOp for Plat {
    fn irq_set_enable(_irq: rdrive::IrqId, _enable: bool) {}

    fn irq_handler() -> someboot::irq::IrqId {
        todo!()
    }

    fn systick_irq() -> rdrive::IrqId {
        someboot::irq::systimer_irq().raw().into()
    }

    fn secondary_init() {}

    fn secondary_init_intc() {}

    fn secondary_init_systick() {}
}
