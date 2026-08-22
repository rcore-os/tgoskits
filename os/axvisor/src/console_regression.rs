//! Deterministic QEMU regressions for host-console ownership.

#[cfg(feature = "test-console-atomic-output")]
pub(crate) fn emit_atomic_output() {
    let no_preempt = ax_std::os::arceos::guard::PreemptGuard::new();
    crate::guest_console::fill_runtime_output_queue();
    crate::guest_console::submit_host_bytes(b"\nCONSOLE_ATOMIC_OUTPUT_REGRESSION_PASSED\n");
    drop(no_preempt);
}

#[cfg(feature = "test-console-interleave")]
pub(crate) fn emit_interleave() {
    ax_api::stdio::ax_console_write_bytes(b"rm")
        .expect("console interleave prefix must be written");
    ax_log::ax_print!(":CONSOLE_INTERLEAVE_HOST_LOG\n");
    ax_api::stdio::ax_console_write_bytes(b"\n")
        .expect("console interleave suffix must be written");
}
