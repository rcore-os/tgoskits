use loongArch64::iocsr::{iocsr_read_w, iocsr_write_w};

use crate::{common::PlatOp, irq::_handle_irq};

mod eiointc;
mod irq_common;
mod pch_pic;

pub struct Plat;

const IOCSR_IPI_SEND_CPU_SHIFT: u32 = 16;
const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;

const IOCSR_IPI_STATUS: usize = 0x1000;
const IOCSR_IPI_ENABLE: usize = 0x1004;
const IOCSR_IPI_CLEAR: usize = 0x100c;
const IOCSR_IPI_SEND: usize = 0x1040;

const EIOINTC_IRQ: usize = 3;
const IPI_IRQ: usize = 12;
const IPI_VECTOR: u32 = 0;

fn make_ipi_send_value(cpu_id: usize, vector: u32, blocking: bool) -> u32 {
    let mut value = (cpu_id as u32) << IOCSR_IPI_SEND_CPU_SHIFT | vector;
    if blocking {
        value |= IOCSR_IPI_SEND_BLOCKING;
    }
    value
}

fn ack_pending_ipi() -> u32 {
    let status = iocsr_read_w(IOCSR_IPI_STATUS);
    if status != 0 {
        iocsr_write_w(IOCSR_IPI_CLEAR, status);
        trace!("IPI status = {status:#x}");
    }
    status
}

impl PlatOp for Plat {
    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        let raw = irq.raw();

        if raw == someboot::irq::systimer_irq().raw() {
            someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
            return;
        }

        if raw == IPI_IRQ {
            let value = if enable { u32::MAX } else { 0 };
            iocsr_write_w(IOCSR_IPI_ENABLE, value);
            someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
            return;
        }

        eiointc::set_irq_enable(raw, enable);
        pch_pic::set_irq_enable(raw, enable);
    }

    fn send_ipi(_irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
        match target {
            crate::irq::IpiTarget::Current { cpu_id } | crate::irq::IpiTarget::Other { cpu_id } => {
                Self::send_ipi_to_cpu(cpu_id);
            }
            crate::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for target_cpu in 0..cpu_num {
                    if target_cpu != cpu_id {
                        Self::send_ipi_to_cpu(target_cpu);
                    }
                }
            }
        }
    }

    fn irq_handler() -> someboot::irq::IrqId {
        someboot::irq::systimer_irq()
    }

    fn irq_handler_with_raw(raw: usize) -> Option<someboot::irq::IrqId> {
        let irq = match raw {
            raw if raw == someboot::irq::systimer_irq().raw() => {
                let irq = someboot::irq::IrqId::new(raw);
                _handle_irq(raw.into());
                someboot::timer::ack();
                irq
            }
            IPI_IRQ => {
                let mut status = ack_pending_ipi();
                let irq = someboot::irq::IrqId::new(raw);
                while status != 0 {
                    let ipi_bit = status.trailing_zeros();
                    status &= !(1 << ipi_bit);
                    _handle_irq(raw.into());
                }
                irq
            }
            EIOINTC_IRQ => {
                let Some(external) = eiointc::claim_irq() else {
                    debug!("Spurious LoongArch EIOINTC interrupt");
                    return None;
                };
                let irq = someboot::irq::IrqId::new(external);
                _handle_irq(external.into());
                eiointc::complete_irq(external);
                irq
            }
            external => {
                let irq = someboot::irq::IrqId::new(external);
                _handle_irq(external.into());
                irq
            }
        };

        Some(irq)
    }

    fn systick_irq() -> rdrive::IrqId {
        someboot::irq::systimer_irq().raw().into()
    }

    fn secondary_init() {}

    fn secondary_init_intc(_cpu_idx: usize) {}

    fn secondary_init_systick() {}

    fn send_ipi_to_cpu(cpu_id: usize) {
        iocsr_write_w(
            IOCSR_IPI_SEND,
            make_ipi_send_value(cpu_id, IPI_VECTOR, false),
        );
    }
}
