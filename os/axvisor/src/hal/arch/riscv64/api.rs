pub(super) fn init_platform_irq_injector() {
    ax_plat_riscv64_qemu_virt::irq::register_virtual_irq_injector(
        crate::hal::arch::inject_interrupt,
    );
}
