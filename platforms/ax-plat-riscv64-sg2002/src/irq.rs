use core::{num::NonZeroU32, ptr::NonNull};

use ax_kspin::SpinNoIrq;
use ax_plat::{
    irq::{
        CPU_LOCAL_IRQ_DOMAIN, HwIrq, IpiTarget, IrqDomainId, IrqError, IrqId, IrqIf, IrqSource,
        RISCV_PLIC_DOMAIN, TrapVector, dispatch_irq,
    },
    percpu::this_cpu_id,
};
use ax_riscv_plic::Plic;
use riscv::register::sie;
use sbi_rt::HartMask;

use crate::config::{devices::PLIC_PADDR, plat::PHYS_VIRT_OFFSET};

/// `Interrupt` bit in `scause`
pub(super) const INTC_IRQ_BASE: usize = 1 << (usize::BITS - 1);

/// Supervisor software interrupt in `scause`
#[allow(unused)]
pub(super) const S_SOFT: usize = INTC_IRQ_BASE + 1;

/// Supervisor timer interrupt in `scause`
pub(super) const S_TIMER: usize = INTC_IRQ_BASE + 5;

/// Supervisor external interrupt in `scause`
pub(super) const S_EXT: usize = INTC_IRQ_BASE + 9;

const PLIC_DOMAIN: IrqDomainId = RISCV_PLIC_DOMAIN;

fn cpu_local_irq(cause: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq((cause & !INTC_IRQ_BASE) as u32))
}

static PLIC: SpinNoIrq<Plic> = SpinNoIrq::new(unsafe {
    Plic::new(NonNull::new((PHYS_VIRT_OFFSET + PLIC_PADDR) as *mut _).unwrap())
});

fn this_context() -> usize {
    let hart_id = this_cpu_id();
    hart_id * 2 + 1 // supervisor context
}

pub(super) fn init_percpu() {
    PLIC.lock().reset_context(this_context());
    // enable soft interrupts, timer interrupts, and external interrupts
    unsafe {
        sie::set_ssoft();
        sie::set_stimer();
        sie::set_sext();
    }
}

macro_rules! with_cause {
    (
        $cause:expr, @S_TIMER =>
        $timer_op:expr, @S_SOFT =>
        $ipi_op:expr, @S_EXT =>
        $ext_op:expr, @EX_IRQ =>
        $plic_op:expr $(,)?
    ) => {
        match $cause {
            S_TIMER => $timer_op,
            S_SOFT => $ipi_op,
            S_EXT => $ext_op,
            other => {
                if other & INTC_IRQ_BASE == 0 {
                    // Device-side interrupts read from PLIC
                    $plic_op
                } else {
                    // Other CPU-side interrupts
                    panic!("Unknown IRQ cause: {other}");
                }
            }
        }
    };
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
        if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            match irq.hwirq.0 as usize {
                5 => unsafe {
                    if enabled {
                        sie::set_stimer();
                    } else {
                        sie::clear_stimer();
                    }
                    Ok(())
                },
                1 => Ok(()),
                _ => Err(IrqError::InvalidIrq),
            }
        } else if irq.domain == PLIC_DOMAIN {
            let Some(irq) = NonZeroU32::new(irq.hwirq.0) else {
                return Err(IrqError::InvalidIrq);
            };
            trace!("PLIC set enable: {irq} {enabled}");
            let mut plic = PLIC.lock();
            if enabled {
                plic.set_priority(irq, 6);
                plic.enable(irq, this_context());
            } else {
                plic.disable(irq, this_context());
            }
            Ok(())
        } else {
            Err(IrqError::InvalidIrq)
        }
    }

    fn set_affinity(
        _irq: IrqId,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(vector: TrapVector) -> Option<IrqId> {
        let irq = vector.0;
        with_cause!(
            irq,
            @S_TIMER => {
                trace!("IRQ: timer");
                let irq_id = cpu_local_irq(irq);
                if !dispatch_irq(irq_id).handled {
                    warn!("Unhandled IRQ: timer");
                }
                Some(irq_id)
            },
            @S_SOFT => {
                trace!("IRQ: IPI");
                let irq_id = cpu_local_irq(irq);
                if !dispatch_irq(irq_id).handled {
                    warn!("Unhandled IRQ: IPI");
                }
                Some(irq_id)
            },
            @S_EXT => {
                let mut plic = PLIC.lock();
                let Some(irq) = plic.claim(this_context()) else {
                    debug!("Spurious external IRQ");
                    return None;
                };
                trace!("IRQ: external {irq}");
                drop(plic);
                let irq_id = IrqId::new(PLIC_DOMAIN, HwIrq(irq.get()));
                if !dispatch_irq(irq_id).handled {
                    debug!("Unhandled external IRQ {irq}");
                }
                PLIC.lock().complete(this_context(), irq);
                Some(irq_id)
            },
            @EX_IRQ => {
                unreachable!("Device-side IRQs should be handled by triggering the External Interrupt.");
            }
        )
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(_irq_num: IrqId, target: IpiTarget) {
        match target {
            IpiTarget::Current { cpu_id } => {
                let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << cpu_id, 0));
                if res.is_err() {
                    warn!("send_ipi failed: {res:?}");
                }
            }
            IpiTarget::Other { cpu_id } => {
                let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << cpu_id, 0));
                if res.is_err() {
                    warn!("send_ipi failed: {res:?}");
                }
            }
            IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for i in 0..cpu_num {
                    if i != cpu_id {
                        let res = sbi_rt::send_ipi(HartMask::from_mask_base(1 << i, 0));
                        if res.is_err() {
                            warn!("send_ipi_all_others failed: {res:?}");
                        }
                    }
                }
            }
        }
    }

    fn ipi_irq() -> IrqId {
        cpu_local_irq(S_SOFT)
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq } if domain == PLIC_DOMAIN => {
                Ok(IrqId::new(domain, hwirq))
            }
            IrqSource::ControllerLine { .. } => Err(IrqError::InvalidIrq),
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
        }
    }

    fn resolve_percpu(hwirq: HwIrq) -> Result<IrqId, IrqError> {
        Ok(IrqId::new(CPU_LOCAL_IRQ_DOMAIN, hwirq))
    }
}
