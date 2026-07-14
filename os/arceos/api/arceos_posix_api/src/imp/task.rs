use core::ffi::c_int;

/// Relinquish the CPU, and switches to another task.
///
/// For single-threaded configuration (`multitask` feature is disabled), we just
/// relax the CPU and wait for incoming interrupts.
#[track_caller]
pub fn sys_sched_yield() -> c_int {
    #[cfg(feature = "multitask")]
    {
        syscall_body!(sys_sched_yield, {
            ax_runtime::task::yield_current_cpu().map_err(|error| {
                warn!("failed to yield current task: {error}");
                ax_errno::LinuxError::EAGAIN
            })?;
            Ok(0)
        })
    }
    #[cfg(not(feature = "multitask"))]
    {
        if cfg!(feature = "irq") {
            ax_hal::asm::wait_for_irqs();
        } else {
            core::hint::spin_loop();
        }
        0
    }
}

/// Get current thread ID.
pub fn sys_getpid() -> c_int {
    syscall_body!(sys_getpid,
        #[cfg(feature = "multitask")]
        {
            let id = ax_runtime::task::current_thread_id().map_err(|error| {
                warn!("failed to read current task identity: {error}");
                ax_errno::LinuxError::EAGAIN
            })?;
            Ok(id.as_u64() as c_int)
        }
        #[cfg(not(feature = "multitask"))]
        {
            Ok(2) // `main` task ID
        }
    )
}

/// Exit current task
#[track_caller]
pub fn sys_exit(exit_code: c_int) -> ! {
    debug!("sys_exit <= {exit_code}");
    #[cfg(feature = "multitask")]
    ax_runtime::task::exit_current(exit_code);
    #[cfg(not(feature = "multitask"))]
    ax_hal::power::system_off();
}
