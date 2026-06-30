//! LoongArch hypervisor IRQ routing extension.

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
    );

    /// Removes all guest IRQ routes owned by one VM.
    fn unregister_guest_irq_routes(vm_id: usize);
}
