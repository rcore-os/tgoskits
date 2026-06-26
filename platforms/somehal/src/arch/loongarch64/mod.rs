use loongArch64::iocsr::{iocsr_read_w, iocsr_write_w};
use rdif_intc::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource},
};

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

fn cpu_local_irq(raw: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(raw as u32))
}

fn pch_pic_irq(input: usize) -> IrqId {
    let domain = crate::irq::domain_by_kind(crate::irq::IrqDomainKind::LoongArchPchPic)
        .expect("LoongArch PCH-PIC IRQ domain is not registered")
        .id;
    IrqId::new(domain, HwIrq(input as u32))
}

fn eiointc_irq(external: usize) -> IrqId {
    let domain = crate::irq::domain_by_kind(crate::irq::IrqDomainKind::LoongArchEioIntc)
        .expect("LoongArch EIOINTC IRQ domain is not registered")
        .id;
    IrqId::new(domain, HwIrq(external as u32))
}

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

fn resolve_acpi_gsi(gsi: u32) -> Result<IrqId, IrqError> {
    let route = rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_gsi(gsi))
        .flatten()
        .ok_or(IrqError::InvalidIrq)?;

    resolve_acpi_route(route)
}

fn resolve_acpi_route(route: AcpiGsiRoute) -> Result<IrqId, IrqError> {
    match route.controller {
        AcpiGsiController::PchPic => {
            pch_pic::setup_acpi_route(&route).ok_or(IrqError::Unsupported)?;
            Ok(pch_pic_irq(usize::from(route.controller_input)))
        }
        AcpiGsiController::IoApic => Err(IrqError::Unsupported),
    }
}

fn route_to_rdif(route: irq_framework::AcpiGsiRoute) -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: route.gsi,
        vector: route.vector,
        controller: match route.controller {
            irq_framework::AcpiGsiController::IoApic => rdif_intc::AcpiGsiController::IoApic,
            irq_framework::AcpiGsiController::PchPic => rdif_intc::AcpiGsiController::PchPic,
        },
        controller_id: route.controller_id,
        controller_address: route.controller_address,
        controller_input: route.controller_input,
        trigger: match route.trigger {
            irq_framework::AcpiIrqTrigger::Edge => AcpiIrqTrigger::Edge,
            irq_framework::AcpiIrqTrigger::Level => AcpiIrqTrigger::Level,
        },
        polarity: match route.polarity {
            irq_framework::AcpiIrqPolarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
            irq_framework::AcpiIrqPolarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
        },
    }
}

impl PlatOp for Plat {
    type ActiveIrq = ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            let raw = irq.hwirq.0 as usize;
            if raw == someboot::irq::systimer_irq().raw() {
                someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
                return Ok(());
            }
            if raw == IPI_IRQ {
                let value = if enable { u32::MAX } else { 0 };
                iocsr_write_w(IOCSR_IPI_ENABLE, value);
                someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
                return Ok(());
            }
            return Err(IrqError::InvalidIrq);
        }

        if crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::LoongArchPchPic)
            || crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::LoongArchEioIntc)
        {
            crate::irq::set_controller_irq_enabled(irq, enable)
        } else {
            Err(IrqError::InvalidIrq)
        }
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Err(IrqError::Unsupported);
        }
        if !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::LoongArchPchPic)
            && !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::LoongArchEioIntc)
        {
            return Err(IrqError::InvalidIrq);
        }
        match affinity {
            crate::irq::IrqAffinity::Any | crate::irq::IrqAffinity::Fixed { cpu_id: 0 } => Ok(()),
            crate::irq::IrqAffinity::Fixed { .. } => Err(IrqError::Unsupported),
        }
    }

    fn send_ipi(_irq: IrqId, target: crate::irq::IpiTarget) {
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

    fn ipi_irq() -> IrqId {
        cpu_local_irq(IPI_IRQ)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        match raw {
            raw if raw == someboot::irq::systimer_irq().raw() => {
                // Clear the current timer interrupt before dispatching. The
                // dispatch path reprograms the next one-shot timer; clearing
                // afterwards can drop a newly-arrived timer edge and strand
                // timer-based sleeps.
                someboot::timer::ack();
                Some(ActiveIrq::new(cpu_local_irq(raw), Completion::None))
            }
            IPI_IRQ => {
                let _status = ack_pending_ipi();
                Some(ActiveIrq::new(cpu_local_irq(raw), Completion::None))
            }
            EIOINTC_IRQ => {
                let Some(external) = eiointc::claim_irq() else {
                    debug!("Spurious LoongArch EIOINTC interrupt");
                    return None;
                };
                let irq = pch_pic::input_for_vector(external)
                    .map(pch_pic_irq)
                    .unwrap_or_else(|| eiointc_irq(external));
                Some(ActiveIrq::new(irq, Completion::EioIntc { irq: external }))
            }
            external => {
                let input = pch_pic::input_for_vector(external).unwrap_or(external);
                Some(ActiveIrq::new(pch_pic_irq(input), Completion::None))
            }
        }
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        active.id()
    }

    fn systick_irq() -> IrqId {
        cpu_local_irq(someboot::irq::systimer_irq().raw())
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq }
                if crate::irq::domain_is_kind(
                    domain,
                    crate::irq::IrqDomainKind::LoongArchPchPic,
                ) || crate::irq::domain_is_kind(
                    domain,
                    crate::irq::IrqDomainKind::LoongArchEioIntc,
                ) || domain == CPU_LOCAL_IRQ_DOMAIN =>
            {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(gsi) => resolve_acpi_gsi(gsi),
            IrqSource::AcpiGsiRoute(route) => resolve_acpi_route(route_to_rdif(route)),
        }
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
    irq: IrqId,
    completion: Completion,
}

impl ActiveIrq {
    const fn new(irq: IrqId, completion: Completion) -> Self {
        Self { irq, completion }
    }

    pub fn id(&self) -> IrqId {
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
