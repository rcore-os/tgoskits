#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
struct PlatformIrqInjector;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
#[ax_crate_interface::impl_interface]
impl axvm::irq::PlatformIrqInjectorIf for PlatformIrqInjector {
    fn register_virtual_irq_injector(injector: fn(ax_hal::irq::IrqId) -> bool) {
        axplat_dyn::register_virtual_irq_injector(injector);
    }
}
