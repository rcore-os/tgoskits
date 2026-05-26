#[ax_runtime::hal::cpu::trap::breakpoint_handler]
fn default_breakpoint_handler(_tf: &mut ax_runtime::hal::cpu::TrapFrame) -> bool {
    false
}

#[cfg(target_arch = "x86_64")]
#[ax_runtime::hal::cpu::trap::debug_handler]
fn default_debug_handler(_tf: &mut ax_runtime::hal::cpu::TrapFrame) -> bool {
    false
}
