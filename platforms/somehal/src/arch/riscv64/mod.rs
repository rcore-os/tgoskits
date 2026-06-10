use crate::common::PlatOp;

mod plic;

pub struct Plat;

pub fn register_current_cpu_id(cpu_idx: usize, reader: fn() -> usize) {
    plic::register_current_cpu_id(cpu_idx, reader);
}

pub(crate) fn claim_external_irq() -> Option<someboot::irq::IrqId> {
    plic::claim_external_irq()
}

pub(crate) fn complete_external_irq(irq: someboot::irq::IrqId) {
    plic::complete_external_irq(irq);
}

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
