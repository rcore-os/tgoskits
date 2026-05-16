use crate::common::PlatOp;

pub struct Plat;

impl PlatOp for Plat {
    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        let raw = irq.into();
        let irq = someboot::irq::IrqId::new(raw);

        if irq == someboot::irq::systimer_irq() {
            someboot::irq::irq_set_enable(irq, enable);
        }
    }

    fn irq_handler() -> someboot::irq::IrqId {
        someboot::irq::systimer_irq()
    }

    fn systick_irq() -> rdrive::IrqId {
        someboot::irq::systimer_irq().raw().into()
    }

    fn secondary_init() {}

    fn secondary_init_intc() {}

    fn secondary_init_systick() {}
}
