#[ax_runtime::hal::cpu::trap::breakpoint_handler]
fn default_breakpoint_handler(tf: &mut ax_runtime::hal::cpu::TrapFrame) -> bool {
    crate::kprobe::handle_breakpoint(tf)
}

#[cfg(target_arch = "x86_64")]
#[ax_runtime::hal::cpu::trap::debug_handler]
fn default_debug_handler(tf: &mut ax_runtime::hal::cpu::TrapFrame) -> bool {
    crate::kprobe::handle_debug(tf)
}
