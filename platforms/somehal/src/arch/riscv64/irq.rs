//! RISC-V trap transactions, local IRQ inputs, and SBI IPI delivery.
use core::num::NonZeroU32;

use riscv::register::{sie, sip};
use sbi_rt::HartMask;

use super::{aia, plic, plic::PlicClaim};
use crate::irq_routing::{
    RISCV_S_EXT_IRQ, RISCV_S_SOFT_IRQ, RISCV_S_TIMER_IRQ, RiscvTrapIrq, classify_riscv_trap,
};

enum Completion {
    None,
    Plic(PlicClaim),
    Aplic(AplicClaim),
}

/// Controller-specific completion owned exclusively by SomeHAL.
#[derive(Debug)]
pub(crate) enum ExternalIrqClaim {
    Plic { context: usize, source: NonZeroU32 },
    Aplic { source: NonZeroU32 },
}

impl ExternalIrqClaim {
    pub(crate) const fn source(&self) -> u32 {
        match self {
            Self::Plic { source, .. } | Self::Aplic { source } => source.get(),
        }
    }
}

enum ActiveIrqSource {
    Legacy(rdrive::IrqId),
    Framework(crate::irq::IrqId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AplicClaim {
    source: NonZeroU32,
}

impl AplicClaim {
    const fn new(source: NonZeroU32) -> Self {
        Self { source }
    }
}

pub struct ActiveIrq {
    irq: ActiveIrqSource,
    completion: Completion,
}

impl ActiveIrq {
    pub(super) fn id(&self) -> rdrive::IrqId {
        match self.irq {
            ActiveIrqSource::Legacy(irq) => irq,
            ActiveIrqSource::Framework(irq) => (irq.hwirq.0 as usize).into(),
        }
    }

    pub(super) const fn framework_id(&self) -> Option<crate::irq::IrqId> {
        match self.irq {
            ActiveIrqSource::Legacy(_) => None,
            ActiveIrqSource::Framework(irq) => Some(irq),
        }
    }

    pub(super) fn take_external_claim(&mut self) -> Option<crate::irq::RiscvExternalIrqClaim> {
        if let Completion::Aplic(claim) = &self.completion
            && !aia::defer(claim.source)
        {
            return None;
        }
        match core::mem::replace(&mut self.completion, Completion::None) {
            Completion::None => None,
            Completion::Plic(claim) => {
                let (context, source) = claim.into_parts();
                Some(crate::irq::RiscvExternalIrqClaim::new(
                    ExternalIrqClaim::Plic { context, source },
                ))
            }
            Completion::Aplic(claim) => Some(crate::irq::RiscvExternalIrqClaim::new(
                ExternalIrqClaim::Aplic {
                    source: claim.source,
                },
            )),
        }
    }

    pub(super) fn take_plic_claim(&mut self) -> Option<PlicClaim> {
        match core::mem::replace(&mut self.completion, Completion::None) {
            Completion::None => None,
            Completion::Plic(claim) => Some(claim),
            Completion::Aplic(claim) => {
                self.completion = Completion::Aplic(claim);
                None
            }
        }
    }
}

impl Drop for ActiveIrq {
    fn drop(&mut self) {
        if let Some(claim) = self.take_plic_claim() {
            plic::complete_claim(claim);
        } else if let Completion::Aplic(claim) =
            core::mem::replace(&mut self.completion, Completion::None)
        {
            aia::complete(claim.source);
        }
    }
}

pub(super) fn begin_irq(raw: usize) -> Option<ActiveIrq> {
    match classify_riscv_trap(raw) {
        RiscvTrapIrq::Timer => Some(ActiveIrq {
            irq: ActiveIrqSource::Legacy(RISCV_S_TIMER_IRQ.into()),
            completion: Completion::None,
        }),
        RiscvTrapIrq::Ipi => {
            // SAFETY: acknowledge only this hart's supervisor software IRQ;
            // the trap transaction owns this pending notification.
            unsafe {
                sip::clear_ssoft();
            }
            Some(ActiveIrq {
                irq: ActiveIrqSource::Legacy(RISCV_S_SOFT_IRQ.into()),
                completion: Completion::None,
            })
        }
        RiscvTrapIrq::External => begin_external_irq(),
        RiscvTrapIrq::UnknownInterrupt { cause } => {
            warn!("unsupported RISC-V interrupt cause {cause}");
            None
        }
        RiscvTrapIrq::BareSource(source) => {
            warn!("ignore bare RISC-V PLIC source {source} outside external interrupt claim path");
            None
        }
    }
}

pub(super) fn systick_irq() -> rdrive::IrqId {
    RISCV_S_TIMER_IRQ.into()
}

pub(super) fn local_irq_set_enable(
    irq: rdrive::IrqId,
    enable: bool,
) -> Result<(), crate::irq::IrqError> {
    let raw: usize = irq.into();
    match raw {
        // SAFETY: only changes this hart's supervisor timer IRQ input.
        RISCV_S_TIMER_IRQ => unsafe {
            if enable {
                sie::set_stimer();
            } else {
                sie::clear_stimer();
            }
            Ok(())
        },
        // SAFETY: only changes this hart's supervisor software IRQ input.
        RISCV_S_SOFT_IRQ => unsafe {
            if enable {
                sie::set_ssoft();
            } else {
                sie::clear_ssoft();
            }
            Ok(())
        },
        // SAFETY: only changes this hart's supervisor external IRQ input.
        RISCV_S_EXT_IRQ => unsafe {
            if enable {
                sie::set_sext();
            } else {
                sie::clear_sext();
            }
            Ok(())
        },
        other => {
            warn!("unsupported RISC-V local IRQ {other:#x}");
            Err(crate::irq::IrqError::InvalidIrq)
        }
    }
}

pub(super) fn send_ipi_to_cpu(cpu_id: usize) -> Result<(), crate::irq::IrqError> {
    let hart_id = someboot::smp::cpu_idx_to_id(cpu_id).ok_or(crate::irq::IrqError::InvalidCpu)?;
    // An SBI IPI is only a doorbell. Complete the shared-memory publication
    // before entering firmware, whose later MMIO/IMSIC operation may otherwise
    // become visible to the target hart first under RVWMO.
    ax_cpu::barrier::data_fence();
    let res = sbi_rt::send_ipi(HartMask::from_mask_base(1, hart_id));
    if res.is_ok() {
        Ok(())
    } else {
        Err(crate::irq::IrqError::Controller)
    }
}

pub(super) fn init_secondary_cpu(cpu_idx: usize) {
    plic::init_cpu_context(cpu_idx);
    aia::init_current_imsic();
    enable_local_irq_inputs();
}

/// Opens the current hart's IRQ inputs without enabling global SIE.
pub(super) fn enable_local_irq_inputs() {
    // SAFETY: only changes local supervisor IRQ inputs after controller setup.
    unsafe {
        sie::set_ssoft();
        sie::set_stimer();
        sie::set_sext();
    }
}

fn begin_external_irq() -> Option<ActiveIrq> {
    if let Some(claim) = plic::claim_external_irq() {
        let (_, source) = claim.into_parts();
        return Some(ActiveIrq {
            irq: ActiveIrqSource::Legacy((source.get() as usize).into()),
            completion: Completion::Plic(claim),
        });
    }

    let claim = AplicClaim::new(aia::claim_external_irq()?);
    let domain = crate::irq::domain_by_kind_fast(crate::irq::IrqDomainKind::RiscvAplic)?;
    Some(ActiveIrq {
        irq: ActiveIrqSource::Framework(crate::irq::IrqId::new(
            domain,
            crate::irq::HwIrq(claim.source.get()),
        )),
        completion: Completion::Aplic(claim),
    })
}
