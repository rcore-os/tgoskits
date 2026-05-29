#[cfg(not(feature = "dyn-plat"))]
compile_error!("riscv64 Axvisor requires the dyn-plat feature");

pub(super) fn init_platform_irq_injector() {
    axplat_dyn::register_virtual_irq_injector(crate::host::irq::inject_interrupt);
}
