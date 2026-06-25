use crate::common::PlatOp;

mod plic;

pub struct Plat;

impl PlatOp for Plat {
    type ActiveIrq = plic::ActiveIrq;

    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        plic::irq_set_enable(irq, enable);
    }

    fn irq_set_affinity(
        irq: rdrive::IrqId,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<(), &'static str> {
        plic::irq_set_affinity(irq, affinity)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        plic::begin_irq(raw)
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> rdrive::IrqId {
        active.id()
    }

    fn systick_irq() -> rdrive::IrqId {
        plic::systick_irq()
    }

    fn secondary_init() {}

    fn secondary_init_intc(cpu_idx: usize) {
        plic::secondary_init_intc(cpu_idx);
    }

    fn secondary_init_systick() {}

    fn send_ipi(_irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
        match target {
            crate::irq::IpiTarget::Current { cpu_id } | crate::irq::IpiTarget::Other { cpu_id } => {
                plic::send_ipi_to_cpu(cpu_id);
            }
            crate::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for target_cpu in 0..cpu_num {
                    if target_cpu != cpu_id {
                        plic::send_ipi_to_cpu(target_cpu);
                    }
                }
            }
        }
    }

    fn send_ipi_to_cpu(cpu_id: usize) {
        plic::send_ipi_to_cpu(cpu_id);
    }
}
