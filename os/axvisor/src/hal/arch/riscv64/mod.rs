mod api;

pub fn prepare_virtualization() {
    axplat_dyn::register_virtual_irq_injector(
        axvisor_core::arch::riscv64::inject_current_interrupt,
    );
}
