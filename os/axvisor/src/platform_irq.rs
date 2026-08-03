#[cfg(target_arch = "aarch64")]
struct Aarch64PlatformIrqInjector;

#[cfg(target_arch = "aarch64")]
#[ax_crate_interface::impl_interface]
impl axvm::irq::Aarch64PlatformIrqInjectorIf for Aarch64PlatformIrqInjector {
    fn register_hardware_irq_injector(injector: fn(usize) -> bool) {
        axplat_dyn::register_aarch64_hardware_irq_injector(injector);
    }
}

#[cfg(target_arch = "riscv64")]
struct RiscvPlatformIrqInjector;

#[cfg(target_arch = "riscv64")]
#[ax_crate_interface::impl_interface]
impl axvm::irq::RiscvPlatformIrqInjectorIf for RiscvPlatformIrqInjector {
    fn register_virtual_irq_injector(injector: fn(usize) -> bool) {
        axplat_dyn::register_virtual_irq_injector(injector);
    }

    fn set_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]) {
        axplat_dyn::set_virtual_irq_targets(cpu_id, irq_sources);
    }
}
