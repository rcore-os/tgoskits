use ax_std::os::arceos::modules;

pub fn handle_irq(vector: usize) {
    modules::ax_hal::trap::irq_handler(vector);
}

pub fn inject_interrupt(vector: usize) {
    crate::host::arch::inject_interrupt(vector as _);
}

#[cfg(target_arch = "x86_64")]
pub fn register(vector: usize, handler: fn(usize)) -> bool {
    modules::ax_hal::irq::register(vector, handler)
}

#[cfg(target_arch = "x86_64")]
pub fn register_irq_hook(handler: fn(usize)) -> bool {
    modules::ax_hal::irq::register_irq_hook(handler)
}
