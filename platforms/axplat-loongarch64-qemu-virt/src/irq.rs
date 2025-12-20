use axplat::irq::{HandlerTable, IpiTarget, IrqHandler, IrqIf};
use loongArch64::{
    iocsr::{iocsr_read_w, iocsr_write_w},
    register::{
        ecfg::{self, LineBasedInterrupt},
        ticlr,
    },
};

use crate::config::devices::{IPI_IRQ, TIMER_IRQ};

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

fn handle_ipi(irq: usize) {
    let mut status = iocsr_read_w(IOCSR_IPI_STATUS);
    if status == 0 {
        return;
    }
    iocsr_write_w(IOCSR_IPI_CLEAR, status);
    trace!("IPI status = {:#x}", status);
    while status != 0 {
        let vector = status.trailing_zeros() as usize;
        status &= !(1 << vector);
        if !IRQ_HANDLER_TABLE.handle(irq) {
            warn!("Unhandled IRQ {}", irq);
        }
    }
}

static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_num: usize, enabled: bool) {
        let interrupt_bit = match irq_num {
            TIMER_IRQ => LineBasedInterrupt::TIMER,
            IPI_IRQ => {
                let value = if enabled { u32::MAX } else { 0 };
                iocsr_write_w(IOCSR_IPI_ENABLE, value);
                LineBasedInterrupt::IPI
            }
            _ => {
                warn!("set_enable: unsupported irq {}", irq_num);
                return;
            }
        };
        let old_value = ecfg::read().lie();
        let new_value = match enabled {
            true => old_value | interrupt_bit,
            false => old_value & !interrupt_bit,
        };
        ecfg::set_lie(new_value);
    }

    /// Registers an IRQ handler for the given IRQ.
    fn register(irq_num: usize, handler: IrqHandler) -> bool {
        if IRQ_HANDLER_TABLE.register_handler(irq_num, handler) {
            Self::set_enable(irq_num, true);
            return true;
        }
        warn!("register handler for IRQ {} failed", irq_num);
        false
    }

    /// Unregisters the IRQ handler for the given IRQ.
    ///
    /// It also disables the IRQ if the unregistration succeeds. It returns the
    /// existing handler if it is registered, `None` otherwise.
    fn unregister(irq: usize) -> Option<IrqHandler> {
        Self::set_enable(irq, false);
        IRQ_HANDLER_TABLE.unregister_handler(irq)
    }

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. It should look up in the
    /// IRQ handler table and calls the corresponding handler. If necessary, it
    /// also acknowledges the interrupt controller after handling.
    fn handle(irq: usize) {
        if irq == IPI_IRQ {
            handle_ipi(irq);
        } else {
            if irq == TIMER_IRQ {
                ticlr::clear_timer_interrupt();
            }
            trace!("IRQ {}", irq);
            if !IRQ_HANDLER_TABLE.handle(irq) {
                warn!("Unhandled IRQ {}", irq);
            }
        }
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
