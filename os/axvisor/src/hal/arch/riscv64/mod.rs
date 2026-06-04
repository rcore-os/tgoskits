mod api;

pub fn prepare_virtualization() {
    api::init_platform_irq_injector();
}
