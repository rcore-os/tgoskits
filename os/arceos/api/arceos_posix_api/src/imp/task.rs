use core::ffi::c_int;

/// Relinquish the CPU, and switches to another task.
///
/// For single-threaded configuration (`multitask` feature is disabled), we just
/// relax the CPU and wait for incoming interrupts.
pub fn sys_sched_yield() -> c_int {
    #[cfg(feature = "multitask")]
    ax_task::yield_now();
    #[cfg(not(feature = "multitask"))]
    if cfg!(feature = "irq") {
        ax_hal::asm::wait_for_irqs();
    } else {
        core::hint::spin_loop();
    }
    0
}

/// Get current thread ID.
pub fn sys_getpid() -> c_int {
    syscall_body!(sys_getpid,
        #[cfg(feature = "multitask")]
        {
            Ok(ax_task::current().id().as_u64() as c_int)
        }
        #[cfg(not(feature = "multitask"))]
        {
            Ok(2) // `main` task ID
        }
    )
}

/// Exit current task
pub fn sys_exit(exit_code: c_int) -> ! {
    debug!("sys_exit <= {exit_code}");
    #[cfg(feature = "multitask")]
    ax_task::exit(exit_code);
    #[cfg(not(feature = "multitask"))]
    ax_hal::power::system_off();
}
