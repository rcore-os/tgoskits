/// The interfaces which the underlying software (kernel or hypervisor) must implement.
pub trait AxVCpuHal {
    /// Memory management interfaces.
    type MmHal: axaddrspace::AxMmHal;

    /// Fetches current interrupt (IRQ) number.
    ///
    /// # Returns
    ///
    /// * `usize` - The current IRQ number.
    fn irq_fetch() -> usize {
        0
    }

    /// Dispatch an interrupt request (IRQ) to the underlying host OS.
    fn irq_hanlder() {
        unimplemented!("irq_handler is not implemented");
    }
}
