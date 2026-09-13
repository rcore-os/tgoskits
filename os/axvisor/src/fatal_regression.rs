//! A real EL2 fault must bypass the replaceable Rust panic hook.

pub(super) fn run() {
    use ax_std::os::arceos::modules::{ax_hal, ax_runtime};

    std::panic::set_hook(Box::new(|_| {
        ax_runtime::emergency_console::write_fmt(format_args!("EL2_FATAL_ENTERED_STD_PANIC\n"));
        ax_hal::power::system_off();
    }));
    // AxVM has installed the guest vector on every CPU. This synchronous
    // exception must use the runtime's terminal path even when an application
    // panic hook would allocate, lock, or recursively fault.
    // SAFETY: this dedicated test intentionally raises a fatal EL2 breakpoint;
    // it does not access memory and expects the runtime to terminate the system.
    unsafe {
        #[cfg(feature = "test-el2-fatal-foreign-context")]
        core::arch::asm!(
            "msr daifset, #0xf",
            "msr tpidr_el0, xzr",
            "msr sp_el0, xzr",
            "brk #0x2356",
            options(noreturn)
        );
        #[cfg(not(feature = "test-el2-fatal-foreign-context"))]
        core::arch::asm!("brk #0x2356", options(nomem, nostack));
    }
    #[cfg(not(feature = "test-el2-fatal-foreign-context"))]
    panic!("EL2 fatal exception unexpectedly returned");
}
