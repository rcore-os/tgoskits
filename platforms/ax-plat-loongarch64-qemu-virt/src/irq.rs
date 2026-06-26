use ax_plat::irq::{
    CPU_LOCAL_IRQ_DOMAIN, HwIrq, IpiTarget, IrqError, IrqId, IrqIf, IrqSource,
    LOONGARCH_EIOINTC_DOMAIN, LOONGARCH_PCH_PIC_DOMAIN, TrapVector, dispatch_irq,
};
use loongArch64::{
    iocsr::{iocsr_read_w, iocsr_write_w},
    register::{
        ecfg::{self, LineBasedInterrupt},
        ticlr,
    },
};

use crate::config::devices::{EIOINTC_IRQ, IPI_IRQ, TIMER_IRQ};

// TODO: move these modules to a separate crate
mod eiointc;
mod pch_pic;

const IOCSR_IPI_SEND_CPU_SHIFT: u32 = 16;
const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;

// [Loongson 3A5000 Manual](https://loongson.github.io/LoongArch-Documentation/Loongson-3A5000-usermanual-EN.html)
// See Section 10.2 for details about IPI registers
const IOCSR_IPI_STATUS: usize = 0x1000;
const IOCSR_IPI_ENABLE: usize = 0x1004;
const IOCSR_IPI_CLEAR: usize = 0x100c;
const IOCSR_IPI_SEND: usize = 0x1040;

fn make_ipi_send_value(cpu_id: usize, vector: u32, blocking: bool) -> u32 {
    let mut value = (cpu_id as u32) << IOCSR_IPI_SEND_CPU_SHIFT | vector;
    if blocking {
        value |= IOCSR_IPI_SEND_BLOCKING;
    }
    value
}

pub(crate) fn init() {
    eiointc::init();
    pch_pic::init();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IrqType {
    Timer,
    Ipi,
    Io,
    Ex(usize),
}

impl IrqType {
    fn new(irq: usize) -> Self {
        match irq {
            TIMER_IRQ => Self::Timer,
            IPI_IRQ => Self::Ipi,
            EIOINTC_IRQ => Self::Io,
            n => Self::Ex(n),
        }
    }

    fn as_usize(&self) -> usize {
        match self {
            IrqType::Timer => TIMER_IRQ,
            IrqType::Ipi => IPI_IRQ,
            IrqType::Io => EIOINTC_IRQ,
            IrqType::Ex(n) => *n,
        }
    }

    fn as_line(&self) -> Option<LineBasedInterrupt> {
        match self {
            IrqType::Timer => Some(LineBasedInterrupt::TIMER),
            IrqType::Ipi => Some(LineBasedInterrupt::IPI),
            _ => None,
        }
    }
}

fn cpu_local_irq(raw: usize) -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(raw as u32))
}

fn pch_pic_irq_from_vector(vector: usize) -> Option<IrqId> {
    pch_pic::input_for_vector(vector)
        .map(|input| IrqId::new(LOONGARCH_PCH_PIC_DOMAIN, HwIrq(input as u32)))
}

fn vector_for_pch_pic_irq(irq: IrqId) -> Option<usize> {
    if irq.domain == LOONGARCH_PCH_PIC_DOMAIN {
        pch_pic::vector_for_input(irq.hwirq.0 as usize)
    } else {
        None
    }
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError> {
        let irq = if irq.domain == CPU_LOCAL_IRQ_DOMAIN {
            IrqType::new(irq.hwirq.0 as usize)
        } else if irq.domain == LOONGARCH_PCH_PIC_DOMAIN {
            let Some(vector) = vector_for_pch_pic_irq(irq) else {
                return Err(IrqError::InvalidIrq);
            };
            IrqType::Ex(vector)
        } else if irq.domain == LOONGARCH_EIOINTC_DOMAIN {
            IrqType::Ex(irq.hwirq.0 as usize)
        } else {
            return Err(IrqError::InvalidIrq);
        };

        match irq {
            IrqType::Ipi => {
                let value = if enabled { u32::MAX } else { 0 };
                iocsr_write_w(IOCSR_IPI_ENABLE, value);
            }
            IrqType::Ex(irq) => {
                if enabled {
                    eiointc::enable_irq(irq);
                    pch_pic::enable_irq(irq);
                } else {
                    eiointc::disable_irq(irq);
                    pch_pic::disable_irq(irq);
                }
            }
            _ => {}
        }

        if let Some(line) = irq.as_line() {
            let old_value = ecfg::read().lie();
            let new_value = match enabled {
                true => old_value | line,
                false => old_value & !line,
            };
            ecfg::set_lie(new_value);
        }
        Ok(())
    }

    fn set_affinity(
        _irq: IrqId,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(vector: TrapVector) -> Option<IrqId> {
        let mut irq = IrqType::new(vector.0);

        if matches!(irq, IrqType::Io) {
            let Some(ex_irq) = eiointc::claim_irq() else {
                debug!("Spurious external IRQ");
                return None;
            };
            irq = IrqType::Ex(ex_irq);
        }

        trace!("IRQ {irq:?}");

        match irq {
            IrqType::Timer => {
                // Clear the interrupt before dispatching. The timer handler
                // programs the next one-shot event; clearing afterwards can
                // drop a freshly-pending event and leave sleepers blocked.
                ticlr::clear_timer_interrupt();
                if !dispatch_irq(cpu_local_irq(irq.as_usize())).handled {
                    debug!("Unhandled IRQ {irq:?}");
                }
            }
            IrqType::Ipi => {
                let mut status = iocsr_read_w(IOCSR_IPI_STATUS);
                if status != 0 {
                    iocsr_write_w(IOCSR_IPI_CLEAR, status);
                    trace!("IPI status = {:#x}", status);

                    while status != 0 {
                        let vector = status.trailing_zeros() as usize;
                        status &= !(1 << vector);
                        if !dispatch_irq(cpu_local_irq(irq.as_usize())).handled {
                            warn!("Unhandled IRQ {irq:?}");
                        }
                    }
                }
            }
            IrqType::Io | IrqType::Ex(_) => {
                let irq_id = pch_pic_irq_from_vector(irq.as_usize()).unwrap_or_else(|| {
                    IrqId::new(LOONGARCH_EIOINTC_DOMAIN, HwIrq(irq.as_usize() as u32))
                });
                if !dispatch_irq(irq_id).handled {
                    debug!("Unhandled IRQ {irq:?}");
                }
            }
        }

        if let IrqType::Ex(irq) = irq {
            eiointc::complete_irq(irq);
        }

        Some(match irq {
            IrqType::Timer | IrqType::Ipi => cpu_local_irq(irq.as_usize()),
            IrqType::Io | IrqType::Ex(_) => {
                pch_pic_irq_from_vector(irq.as_usize()).unwrap_or_else(|| {
                    IrqId::new(LOONGARCH_EIOINTC_DOMAIN, HwIrq(irq.as_usize() as u32))
                })
            }
        })
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    ///
    /// Runtime IPIs are sent NON-blocking (`IOCSR_IPI_SEND_BLOCKING` unset). The
    /// blocking variant stalls the issuing CPU until the target clears its
    /// `IOCSR_IPI_STATUS`; under a high-rate IPI burst the sender can block while
    /// the target is mid-handler (IRQs disabled) — or while the sender itself holds
    /// an IRQ-disabling lock — which deadlocks (the arceos-ipi SMP test hung 6h on
    /// loongarch). Linux/riscv/x86 likewise fire runtime IPIs non-blocking; the
    /// blocking form is reserved for the secondary-CPU boot mailbox (see `mp.rs`).
    fn send_ipi(_irq_num: IrqId, target: IpiTarget) {
        match target {
            IpiTarget::Current { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, false));
            }
            IpiTarget::Other { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, false));
            }
            IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for i in 0..cpu_num {
                    if i != cpu_id {
                        iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(i, 0, false));
                    }
                }
            }
        }
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq }
                if domain == LOONGARCH_PCH_PIC_DOMAIN
                    || domain == LOONGARCH_EIOINTC_DOMAIN
                    || domain == CPU_LOCAL_IRQ_DOMAIN =>
            {
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
