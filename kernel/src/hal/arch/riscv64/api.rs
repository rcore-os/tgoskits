#[axvisor_api::api_mod_impl(axvisor_api::arch)]
mod arch_api_impl {
    extern fn inject_virtual_interrupt(irq: usize) {
        crate::hal::arch::inject_interrupt(irq);
    }
}
