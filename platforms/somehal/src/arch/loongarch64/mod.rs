use loongArch64::iocsr::{iocsr_read_w, iocsr_write_w};

use crate::common::PlatOp;

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
    type ActiveIrq = ActiveIrq;

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

    fn irq_set_affinity(
        irq: rdrive::IrqId,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<(), &'static str> {
        let raw = irq.raw();
        if raw == someboot::irq::systimer_irq().raw() || raw == IPI_IRQ {
            return Err("LoongArch local IRQ affinity cannot be changed");
        }
        match affinity {
            crate::irq::IrqAffinity::Any | crate::irq::IrqAffinity::Fixed { cpu_id: 0 } => Ok(()),
            crate::irq::IrqAffinity::Fixed { .. } => {
                Err("LoongArch EIOINTC affinity currently supports only CPU0")
            }
        }
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

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        match raw {
            raw if raw == someboot::irq::systimer_irq().raw() => {
                // Clear the current timer interrupt before dispatching. The
                // dispatch path reprograms the next one-shot timer; clearing
                // afterwards can drop a newly-arrived timer edge and strand
                // timer-based sleeps.
                someboot::timer::ack();
                Some(ActiveIrq::new(raw.into(), Completion::None))
            }
            IPI_IRQ => {
                let _status = ack_pending_ipi();
                Some(ActiveIrq::new(raw.into(), Completion::None))
            }
            EIOINTC_IRQ => {
                let Some(external) = eiointc::claim_irq() else {
                    debug!("Spurious LoongArch EIOINTC interrupt");
                    return None;
                };
                Some(ActiveIrq::new(
                    external.into(),
                    Completion::EioIntc { irq: external },
                ))
            }
            external => Some(ActiveIrq::new(external.into(), Completion::None)),
        }
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> rdrive::IrqId {
        active.id()
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

enum Completion {
    None,
    EioIntc { irq: usize },
}

pub struct ActiveIrq {
    irq: rdrive::IrqId,
    completion: Completion,
}

impl ActiveIrq {
    const fn new(irq: rdrive::IrqId, completion: Completion) -> Self {
        Self { irq, completion }
    }

    pub fn id(&self) -> rdrive::IrqId {
        self.irq
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        match self.completion {
            Completion::None => {}
            Completion::EioIntc { irq } => eiointc::complete_irq(irq),
        }
    }
}
