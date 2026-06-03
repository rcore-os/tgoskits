mod api;
pub mod cache;

pub fn inject_interrupt(irq_id: usize) -> bool {
    axvisor_core::arch::riscv64::inject_current_interrupt(irq_id)
}

pub fn prepare_virtualization() {
    api::init_platform_irq_injector();
}
