// The axtest integration root includes this module and supplies the hooks for
// the final kernel image; its automatically linked library copy must not
// publish a second implementation of the same runtime interfaces.
#[cfg(any(not(feature = "axtest"), test))]
#[ax_runtime::hal::cpu::trap::breakpoint_handler]
fn default_breakpoint_handler(tf: &mut ax_runtime::hal::cpu::KernelTrapFrame<'_>) -> bool {
    crate::kprobe::handle_breakpoint(tf)
}

#[cfg(all(
    target_arch = "x86_64",
    any(not(feature = "axtest"), test)
))]
#[ax_runtime::hal::cpu::trap::debug_handler]
fn default_debug_handler(tf: &mut ax_runtime::hal::cpu::KernelTrapFrame<'_>) -> bool {
    crate::kprobe::handle_debug(tf)
}
