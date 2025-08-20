use axplat::irq::{HandlerTable, IpiTarget, IrqHandler, IrqIf};
use loongArch64::register::{
    ecfg::{self, LineBasedInterrupt},
    ticlr,
};

/// The maximum number of IRQs.
pub const MAX_IRQ_COUNT: usize = 12;

static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_num: usize, enabled: bool) {
        if irq_num == crate::config::devices::TIMER_IRQ {
            let old_value = ecfg::read().lie();
            let new_value = match enabled {
                true => old_value | LineBasedInterrupt::TIMER,
                false => old_value & !LineBasedInterrupt::TIMER,
            };
            ecfg::set_lie(new_value);
        }
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
        if irq == crate::config::devices::TIMER_IRQ {
            ticlr::clear_timer_interrupt();
        }
        trace!("IRQ {}", irq);
        if !IRQ_HANDLER_TABLE.handle(irq) {
            warn!("Unhandled IRQ {}", irq);
        }
    }

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(irq_num: usize, target: IpiTarget) {
        todo!()
    }
}
