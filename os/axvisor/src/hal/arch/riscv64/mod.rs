mod api;
pub mod cache;

pub use axvisor_core::arch::riscv64::inject_interrupt;

pub fn prepare_virtualization() {
    api::init_platform_irq_injector();
}
