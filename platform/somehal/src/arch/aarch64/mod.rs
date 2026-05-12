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

    fn irq_handler() -> someboot::irq::IrqId {
        gic::irq_handler()
    }

    fn secondary_init() {}

    fn secondary_init_intc() {
        gic::init_current_cpu();
    }

    fn secondary_init_systick() {
        systick::setup_systick_irq();
    }
}
