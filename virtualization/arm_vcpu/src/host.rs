//! Host callbacks required by AArch64 vCPU implementation.

/// Host architecture operations required by AArch64 virtualization code.
#[ax_crate_interface::def_interface]
pub trait ArmVcpuHostIf {
    /// Inject a virtual interrupt through host GIC state.
    fn hardware_inject_virtual_interrupt(vector: u8);

    /// Handle or report a pending host IRQ.
    ///
    /// Some hosts acknowledge and dispatch the IRQ in this callback and return
    /// a placeholder vector to notify the vCPU loop that an external interrupt
    /// happened.
    fn fetch_irq() -> usize;

    /// Dispatch a host IRQ taken while running at the current exception level.
    fn handle_irq();
}

pub(crate) fn hardware_inject_virtual_interrupt(vector: u8) {
    ax_crate_interface::call_interface!(ArmVcpuHostIf::hardware_inject_virtual_interrupt(vector));
}

pub(crate) fn fetch_irq() -> usize {
    ax_crate_interface::call_interface!(ArmVcpuHostIf::fetch_irq())
}

pub(crate) fn handle_irq() {
    ax_crate_interface::call_interface!(ArmVcpuHostIf::handle_irq());
}
