mod api;
pub mod cache;

pub fn inject_interrupt(irq_id: usize) -> bool {
    if !crate::hal::task::in_vcpu_task_context() {
        return false;
    }
    axvisor_core::arch::riscv64::inject_interrupt(irq_id)
}

pub fn prepare_virtualization() {
    api::init_platform_irq_injector();
}
