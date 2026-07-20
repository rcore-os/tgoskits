use alloc::boxed::Box;

use irq_framework::{CpuId, IrqScope};

use crate::{
    common::PlatOp,
    irq::{CPU_LOCAL_IRQ_DOMAIN, CpuIpiTarget, HwIrq, IpiSendStatus, IrqError, IrqId, IrqSource},
    irq_line::{IrqChipLine, PreparedIrqChipLine},
};

mod plic;

pub use plic::{RiscvPlicIrqEndpoint, RiscvPlicLeaseId};

use crate::irq_routing::{
    RISCV_INTERRUPT_BIT, RISCV_S_SOFT_CAUSE, riscv_cpu_local_hwirq_is_runtime_irq,
    riscv_cpu_local_irq_from_raw, riscv_local_irq_raw, riscv_resolve_controller_line,
};

pub struct Plat;

fn plic_irq_id_from_claimed_source(source: usize) -> Result<IrqId, IrqError> {
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvPlic)
        .ok_or(IrqError::Unsupported)?;
    let source = u32::try_from(source).map_err(|_| IrqError::InvalidIrq)?;
    if source == 0 {
        return Err(IrqError::InvalidIrq);
    }
    Ok(IrqId::new(domain, HwIrq(source)))
}

fn checked_cpu_local_irq(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    if riscv_cpu_local_hwirq_is_runtime_irq(hwirq) {
        Ok(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, hwirq))
    } else {
        Err(IrqError::InvalidIrq)
    }
}

pub fn lease_riscv_plic_irq_endpoint(
    irq: IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<RiscvPlicIrqEndpoint, IrqError> {
    if !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
        return Err(IrqError::InvalidIrq);
    }
    plic::lease_irq_endpoint(irq.hwirq, affinity)
}

pub fn lease_riscv_plic_irq_endpoints(
    irqs: &[IrqId],
    affinity: crate::irq::IrqAffinity,
) -> Result<alloc::vec::Vec<RiscvPlicIrqEndpoint>, IrqError> {
    let mut hwirqs = alloc::vec::Vec::with_capacity(irqs.len());
    for irq in irqs {
        if !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic) {
            return Err(IrqError::InvalidIrq);
        }
        hwirqs.push(irq.hwirq);
    }
    plic::lease_irq_endpoints(&hwirqs, affinity)
}

pub fn release_riscv_plic_irq_endpoints(leases: &[RiscvPlicLeaseId]) -> Result<(), IrqError> {
    plic::release_irq_endpoints(leases)
}

impl PlatOp for Plat {
    type ActiveIrq = plic::ActiveIrq;

    fn prepare_irq_line(
        irq: IrqId,
        scope: IrqScope,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<PreparedIrqChipLine, IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            let raw = riscv_local_irq_raw(irq)?;
            if !matches!(scope, IrqScope::PerCpu { .. }) {
                return Err(IrqError::InvalidIrq);
            }
            return Ok(PreparedIrqChipLine::maskable(Box::new(
                RiscvIrqChipLine::CpuLocal { irq, raw },
            )));
        }
        if !crate::irq::domain_is_kind(irq.domain, crate::irq::IrqDomainKind::RiscvPlic)
            || scope != IrqScope::Global
        {
            return Err(IrqError::InvalidIrq);
        }

        // The lease resolves the generic controller only in task context,
        // fixes its routing, and leaves the physical source priority at zero.
        let endpoint = plic::lease_irq_endpoint(irq.hwirq, affinity)?;
        Ok(PreparedIrqChipLine::maskable(Box::new(
            RiscvIrqChipLine::Plic { irq, endpoint },
        )))
    }

    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq> {
        plic::begin_irq(raw)
    }

    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId {
        let raw: usize = active.id().into();
        if raw & RISCV_INTERRUPT_BIT != 0 {
            riscv_cpu_local_irq_from_raw(raw).expect("active RISC-V local IRQ must be validated")
        } else {
            plic_irq_id_from_claimed_source(raw)
                .expect("active RISC-V PLIC source must come from a validated claim")
        }
    }

    fn systick_irq() -> IrqId {
        riscv_cpu_local_irq_from_raw(plic::systick_irq().into())
            .expect("RISC-V systick IRQ must be a CPU-local timer cause")
    }

    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
        riscv_resolve_controller_line(source, || {
            matches!(
                source,
                IrqSource::ControllerLine { domain, .. }
                    if crate::irq::domain_is_kind(domain, crate::irq::IrqDomainKind::RiscvPlic)
            )
        })?;
        match source {
            IrqSource::ControllerLine { domain, hwirq } if domain == CPU_LOCAL_IRQ_DOMAIN => {
                checked_cpu_local_irq(hwirq)
            }
            IrqSource::ControllerLine { domain, hwirq } => {
                plic::source_from_hwirq(hwirq)?;
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => unreachable!(),
        }
    }

    fn secondary_init() {}

    fn init_boot_irq_cpu(cpu_idx: usize, role: crate::irq::CpuBootRole) -> Result<(), IrqError> {
        match role {
            crate::irq::CpuBootRole::Primary => {}
            crate::irq::CpuBootRole::Secondary => plic::secondary_init_intc(cpu_idx),
        }
        Ok(())
    }

    fn send_ipi(
        irq: IrqId,
        target: CpuIpiTarget,
        current_cpu: irq_framework::CpuId,
    ) -> IpiSendStatus {
        if irq != Self::ipi_irq() {
            return IpiSendStatus::Invalid;
        }
        match target {
            CpuIpiTarget::Current { cpu } => {
                if current_cpu != cpu {
                    return IpiSendStatus::Invalid;
                }
                plic::send_ipi_to_cpu(cpu)
            }
            CpuIpiTarget::Other { cpu } => plic::send_ipi_to_cpu(cpu),
            CpuIpiTarget::AllExceptCurrent { current, cpu_count } => {
                if cpu_count != someboot::smp::runtime_cpu_count() || current_cpu != current {
                    return IpiSendStatus::Invalid;
                }
                // Reject every permanent target error before committing the
                // first SBI transaction; transient Retry remains idempotent.
                if (0..cpu_count)
                    .filter(|cpu| *cpu != current.0)
                    .any(|cpu| plic::checked_ipi_hart_id(irq_framework::CpuId(cpu)).is_none())
                {
                    return IpiSendStatus::Invalid;
                }
                for target_cpu in 0..cpu_count {
                    let target_cpu = irq_framework::CpuId(target_cpu);
                    if target_cpu != current {
                        let status = plic::send_ipi_to_cpu(target_cpu);
                        if status != IpiSendStatus::Success {
                            return status;
                        }
                    }
                }
                IpiSendStatus::Success
            }
        }
    }

    fn ipi_irq() -> IrqId {
        IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(RISCV_S_SOFT_CAUSE as u32))
    }
}

enum RiscvIrqChipLine {
    CpuLocal {
        irq: IrqId,
        raw: usize,
    },
    Plic {
        irq: IrqId,
        endpoint: RiscvPlicIrqEndpoint,
    },
}

// SAFETY: preparation validates CPU-local causes or acquires a shutdown-safe
// PLIC source lease. Live operations touch only the local CSR/SBI leaf or the
// leased priority register and never allocate, block, or resolve a driver.
unsafe impl IrqChipLine for RiscvIrqChipLine {
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool) {
        match self {
            Self::CpuLocal { irq, raw } => {
                let cpu = cpu.expect("prepared RISC-V CPU-local line requires a target CPU");
                assert_eq!(
                    crate::cpu::runtime_current_cpu(),
                    Some(cpu),
                    "prepared RISC-V CPU-local line {irq:?} executed on the wrong CPU"
                );
                plic::local_irq_set_enable((*raw).into(), enabled).unwrap_or_else(|error| {
                    panic!(
                        "fatal platform invariant: prepared RISC-V CPU-local line {irq:?} failed: \
                         {error:?}"
                    )
                });
            }
            Self::Plic { irq, endpoint } => {
                assert!(
                    cpu.is_none(),
                    "prepared RISC-V PLIC line {irq:?} cannot use a per-CPU target"
                );
                if enabled {
                    endpoint.unmask();
                } else {
                    endpoint.mask();
                }
            }
        }
    }

    fn release(&self) -> Result<(), IrqError> {
        match self {
            Self::CpuLocal { .. } => Err(IrqError::Unsupported),
            Self::Plic { endpoint, .. } => {
                let lease = endpoint.lease_id();
                plic::release_irq_endpoints(core::slice::from_ref(&lease))
            }
        }
    }
}
