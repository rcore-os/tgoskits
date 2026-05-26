use crate::common::PlatOp;

mod plic;

pub struct Plat;

impl PlatOp for Plat {
    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        plic::irq_set_enable(irq, enable);
    }

    fn irq_handler() -> someboot::irq::IrqId {
        someboot::irq::IrqId::new(plic::systick_irq().raw())
    }

    fn irq_handler_with_raw(raw: usize) -> Option<someboot::irq::IrqId> {
        plic::irq_handler_with_raw(raw)
    }

    fn systick_irq() -> rdrive::IrqId {
        plic::systick_irq()
    }

    fn secondary_init() {}

    fn secondary_init_intc() {
        plic::secondary_init_intc();
    }

    fn secondary_init_systick() {}

    fn send_ipi_to_cpu(cpu_id: usize) {
        plic::send_ipi_to_cpu(cpu_id);
    }
}
