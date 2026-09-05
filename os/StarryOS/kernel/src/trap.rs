fn default_breakpoint_handler(tf: &mut ax_runtime::hal::cpu::KernelTrapFrame<'_>) -> bool {
    crate::kprobe::handle_breakpoint(tf)
}

#[cfg(target_arch = "x86_64")]
fn default_debug_handler(tf: &mut ax_runtime::hal::cpu::KernelTrapFrame<'_>) -> bool {
    crate::kprobe::handle_debug(tf)
}

pub(crate) fn init_handlers() {
    use ax_runtime::hal::cpu::trap;

    trap::set_page_fault_handler(crate::mm::handle_page_fault);
    trap::set_breakpoint_handler(default_breakpoint_handler);
    #[cfg(target_arch = "x86_64")]
    trap::set_debug_handler(default_debug_handler);
}
