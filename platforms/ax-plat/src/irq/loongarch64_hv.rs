//! LoongArch hypervisor IRQ routing extension.

use super::IrqError;

/// LoongArch hypervisor IRQ routing extension.
#[def_plat_interface]
pub trait LoongArchHvIrqIf {
    /// Registers the virtual interrupt injector used by hypervisor builds.
    fn register_virtual_irq_injector(injector: fn(usize, usize, usize, usize));

    /// Routes one physical EIOINTC/PCH-PIC IRQ to a guest CPU interrupt vector.
    fn register_guest_irq_route(
        physical_irq: usize,
        vm_id: usize,
        vcpu_id: usize,
        guest_vector: usize,
    ) -> Result<(), IrqError>;

    /// Masks and unpublishes all guest IRQ routes owned by one VM.
    fn begin_guest_irq_route_revocation(vm_id: usize) -> Result<(), IrqError>;

    /// Reports whether every callback that observed the old route has exited.
    fn poll_guest_irq_route_revocation(vm_id: usize) -> Result<bool, IrqError>;
}
