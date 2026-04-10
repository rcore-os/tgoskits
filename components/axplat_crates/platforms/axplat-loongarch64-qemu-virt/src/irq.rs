use ax_plat::irq::{HandlerTable, IpiTarget, IrqHandler, IrqIf};
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

/// The maximum number of IRQs.
pub const MAX_IRQ_COUNT: usize = 13;
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

static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

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

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: usize, enabled: bool) {
        let irq = IrqType::new(irq);

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
    }

    /// Registers an IRQ handler for the given IRQ.
    fn register(irq: usize, handler: IrqHandler) -> bool {
        if IRQ_HANDLER_TABLE.register_handler(irq, handler) {
            Self::set_enable(irq, true);
            return true;
        }
        warn!("register handler for IRQ {} failed", irq);
        false
    }

    /// Unregisters the IRQ handler for the given IRQ.
    ///
    /// It also disables the IRQ if the unregistration succeeds. It returns the
    /// existing handler if it is registered, `None` otherwise.
    fn unregister(irq: usize) -> Option<IrqHandler> {
        IRQ_HANDLER_TABLE
            .unregister_handler(irq)
            .inspect(|_| Self::set_enable(irq, false))
    }

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. It should look up in the
    /// IRQ handler table and calls the corresponding handler. If necessary, it
    /// also acknowledges the interrupt controller after handling.
    fn handle(irq: usize) -> Option<usize> {
        let mut irq = IrqType::new(irq);

        if matches!(irq, IrqType::Io) {
            let Some(ex_irq) = eiointc::claim_irq() else {
                debug!("Spurious external IRQ");
                return None;
            };
            irq = IrqType::Ex(ex_irq);
        }

        trace!("IRQ {irq:?}");

        if let IrqType::Ipi = irq {
            let mut status = iocsr_read_w(IOCSR_IPI_STATUS);
            if status != 0 {
                iocsr_write_w(IOCSR_IPI_CLEAR, status);
                trace!("IPI status = {:#x}", status);

                while status != 0 {
                    let vector = status.trailing_zeros() as usize;
                    status &= !(1 << vector);
                    if !IRQ_HANDLER_TABLE.handle(irq.as_usize()) {
                        warn!("Unhandled IRQ {irq:?}");
                    }
                }
            }
        } else {
            if !IRQ_HANDLER_TABLE.handle(irq.as_usize()) {
                debug!("Unhandled IRQ {irq:?}");
            }
        }

        match irq {
            IrqType::Timer => {
                ticlr::clear_timer_interrupt();
            }
            IrqType::Ex(irq) => {
                eiointc::complete_irq(irq);
            }
            _ => {}
        }

        Some(irq.as_usize())
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(_irq_num: usize, target: IpiTarget) {
        match target {
            IpiTarget::Current { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, true));
            }
            IpiTarget::Other { cpu_id } => {
                iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(cpu_id, 0, true));
            }
            IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                for i in 0..cpu_num {
                    if i != cpu_id {
                        iocsr_write_w(IOCSR_IPI_SEND, make_ipi_send_value(i, 0, true));
                    }
                }
            }
        }
    }
}
