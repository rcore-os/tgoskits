use crate::common::PlatOp;

pub mod gic;
pub mod systick;

pub struct Plat;

impl PlatOp for Plat {
    type ActiveIrq = gic::ActiveIrq;

    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        gic::irq_set_enable(irq, enable);
    }

    fn irq_set_affinity(
        irq: rdrive::IrqId,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<(), &'static str> {
        gic::irq_set_affinity(irq, affinity)
    }

    fn send_ipi(irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
        gic::send_ipi(irq, target);
    }

    fn ipi_irq() -> rdrive::IrqId {
        0usize.into()
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        let _ = raw;
        gic::begin_irq()
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> rdrive::IrqId {
        active.id()
    }

    fn systick_irq() -> rdrive::IrqId {
        systick::systick_irq()
    }

    fn secondary_init() {}

    fn secondary_init_intc(cpu_idx: usize) {
        gic::init_cpu(cpu_idx);
    }

    fn secondary_init_systick() {
        systick::setup_systick_irq();
    }
}
