use loongArch64::iocsr::{iocsr_read_w, iocsr_write_w};
use rdif_intc::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, IrqSource},
};

mod eiointc;
mod ipi_command;
mod irq_common;
mod liointc;
mod liointc_cpu_interface;
mod pch_pic;

use crate::irq_routing::{RawIrq, classify_cpu_irq, cpu_local_hwirq_is_runtime_irq};

pub struct Plat;

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

fn checked_cpu_local_irq(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    let raw = hwirq.0 as usize;
    if cpu_local_hwirq_is_runtime_irq(
        raw,
        someboot::irq::systimer_irq().raw(),
        IPI_IRQ,
        EIOINTC_IRQ,
    ) {
        Ok(cpu_local_irq(raw))
    } else {
        Err(IrqError::InvalidIrq)
    }
}

fn eiointc_irq(external: usize) -> IrqId {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::LoongArchEioIntc)
        .expect("LoongArch EIOINTC IRQ domain is not registered");
    IrqId::new(domain, HwIrq(external as u32))
}

fn is_loongarch_external_domain(domain: crate::irq::IrqDomainId) -> bool {
    crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::LoongArchPchPic)
        || crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::LoongArchEioIntc)
        || crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::LoongArchLioIntc)
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
        AcpiGsiController::PchPic => pch_pic::resolve_acpi_route(&route),
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

        if is_loongarch_external_domain(irq.domain) {
            crate::irq::set_controller_irq_enabled(irq, enable)
        } else {
            Err(IrqError::InvalidIrq)
        }
    }

    fn irq_set_affinity(irq: IrqId, affinity: crate::irq::IrqAffinity) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            return Err(IrqError::Unsupported);
        }
        if !is_loongarch_external_domain(irq.domain) {
            return Err(IrqError::InvalidIrq);
        }
        match affinity {
            crate::irq::IrqAffinity::Any | crate::irq::IrqAffinity::Fixed { cpu_id: 0 } => Ok(()),
            crate::irq::IrqAffinity::Fixed { .. } => Err(IrqError::Unsupported),
        }
    }

    fn send_ipi(irq: IrqId, target: crate::irq::IpiTarget) -> Result<(), IrqError> {
        if irq != Self::ipi_irq() {
            return Err(IrqError::InvalidIrq);
        }
        match target {
            crate::irq::IpiTarget::Current => {
                let cpu = crate::cpu::current_cpu_idx().ok_or(IrqError::InvalidCpu)?;
                Self::send_ipi_to_cpu(cpu)
            }
            crate::irq::IpiTarget::Cpu(cpu) => Self::send_ipi_to_cpu(cpu.0),
        }
    }

    fn ipi_irq() -> IrqId {
        cpu_local_irq(IPI_IRQ)
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        if liointc::is_cascade_irq(raw) {
            let Some(irq) = liointc::claim_irq(raw) else {
                debug!("Spurious LoongArch LIOINTC interrupt");
                return None;
            };
            return Some(ActiveIrq::new(irq, Completion::LioIntc { irq }));
        }

        match classify_cpu_irq(
            raw,
            someboot::irq::systimer_irq().raw(),
            IPI_IRQ,
            EIOINTC_IRQ,
        ) {
            RawIrq::Timer => {
                // Clear the current timer interrupt before dispatching. The
                // dispatch path reprograms the next one-shot timer; clearing
                // afterwards can drop a newly-arrived timer edge and strand
                // timer-based sleeps.
                someboot::timer::ack();
                Some(ActiveIrq::new(cpu_local_irq(raw), Completion::None))
            }
            RawIrq::Ipi => {
                let _status = ack_pending_ipi();
                Some(ActiveIrq::new(cpu_local_irq(raw), Completion::None))
            }
            RawIrq::External => {
                let Some(external) = eiointc::claim_irq() else {
                    debug!("Spurious LoongArch EIOINTC interrupt");
                    return None;
                };
                let irq = pch_pic::irq_for_external_vector(external)
                    .unwrap_or_else(|| eiointc_irq(external));
                Some(ActiveIrq::new(irq, Completion::EioIntc { irq: external }))
            }
            RawIrq::Unknown => {
                warn!("unrouted LoongArch CPU interrupt line {raw}");
                None
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
            IrqSource::ControllerLine { domain, hwirq } if is_loongarch_external_domain(domain) => {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { domain, hwirq } if domain == CPU_LOCAL_IRQ_DOMAIN => {
                checked_cpu_local_irq(hwirq)
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(gsi) => resolve_acpi_gsi(gsi),
            IrqSource::AcpiGsiRoute(route) => resolve_acpi_route(route_to_rdif(route)),
        }
    }

    fn secondary_init() {}

    fn init_boot_irq_cpu(_cpu_idx: usize, _role: crate::irq::CpuBootRole) {}

    fn send_ipi_to_cpu(cpu_id: usize) -> Result<(), IrqError> {
        if cpu_id >= someboot::smp::cpu_count() {
            return Err(IrqError::InvalidCpu);
        }
        let command =
            ipi_command::make_ipi_send_value(cpu_id, IPI_VECTOR).ok_or(IrqError::InvalidCpu)?;
        // The blocking command waits for transport acceptance, not for prior
        // shared-memory stores. Complete those stores before ringing the IOCSR
        // doorbell so the target cannot observe a stale payload.
        unsafe {
            core::arch::asm!("dbar 0", options(nostack));
        }
        iocsr_write_w(IOCSR_IPI_SEND, command);
        Ok(())
    }
}

enum Completion {
    None,
    EioIntc { irq: usize },
    LioIntc { irq: IrqId },
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
            Completion::LioIntc { irq } => liointc::complete_irq(irq),
        }
    }
}
