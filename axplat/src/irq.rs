//! Interrupt request (IRQ) handling.

pub use handler_table::HandlerTable;

/// The type if an IRQ handler.
pub type IrqHandler = handler_table::Handler;

/// Target specification for inter-processor interrupts (IPIs).
pub enum IpiTarget {
    /// Send to the current CPU.
    Current {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
    },
    /// Send to a specific CPU.
    Other {
        /// The CPU ID of the target CPU.
        cpu_id: usize,
    },
    /// Send to all other CPUs.
    AllExceptCurrent {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
        /// The total number of CPUs.
        cpu_num: usize,
    },
}

/// IRQ management interface.
#[def_plat_interface]
pub trait IrqIf {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: usize, enabled: bool);

    /// Registers an IRQ handler for the given IRQ.
    ///
    /// It also enables the IRQ if the registration succeeds. It returns `false`
    /// if the registration failed.
    fn register(irq: usize, handler: IrqHandler) -> bool;

    /// Unregisters the IRQ handler for the given IRQ.
    ///
    /// It also disables the IRQ if the unregistration succeeds. It returns the
    /// existing handler if it is registered, `None` otherwise.
    fn unregister(irq: usize) -> Option<IrqHandler>;

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. It should look up in the
    /// IRQ handler table and calls the corresponding handler. If necessary, it
    /// also acknowledges the interrupt controller after handling.
    fn handle(irq: usize);

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(irq_num: usize, target: IpiTarget);
}
