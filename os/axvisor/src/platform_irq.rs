struct RiscvPlatformIrqInjector;

#[ax_crate_interface::impl_interface]
impl axvm::irq::RiscvPlatformIrqInjectorIf for RiscvPlatformIrqInjector {
    fn register_virtual_irq_injector(injector: fn(usize) -> bool) {
        axplat_dyn::register_virtual_irq_injector(injector);
    }

    fn set_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]) {
        axplat_dyn::set_virtual_irq_targets(cpu_id, irq_sources);
    }
}
