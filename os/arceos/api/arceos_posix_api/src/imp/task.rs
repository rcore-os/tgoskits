use core::ffi::c_int;

/// Relinquish the CPU, and switches to another task.
#[track_caller]
pub fn sys_sched_yield() -> c_int {
    {
        syscall_body!(sys_sched_yield, {
            ax_runtime::task::yield_current_cpu().map_err(|error| {
                warn!("failed to yield current task: {error}");
                crate::PosixError::EAGAIN
            })?;
            Ok(0)
        })
    }
}

/// Get current thread ID.
pub fn sys_getpid() -> c_int {
    syscall_body!(sys_getpid,
        {
            let id = ax_runtime::task::current_thread_id().map_err(|error| {
                warn!("failed to read current task identity: {error}");
                crate::PosixError::EAGAIN
            })?;
            Ok(id.as_u64() as c_int)
        }
    )
}

/// Exit current task
#[track_caller]
pub fn sys_exit(exit_code: c_int) -> ! {
    debug!("sys_exit <= {exit_code}");
    ax_runtime::task::exit_current(exit_code);
}
