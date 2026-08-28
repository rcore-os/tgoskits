use core::ffi::c_int;

/// Relinquish the CPU, and switches to another task.
#[track_caller]
pub fn sys_sched_yield() -> c_int {
    ax_task::yield_now();
    0
}

/// Get current thread ID.
pub fn sys_getpid() -> c_int {
    syscall_body!(sys_getpid, {
        Ok(ax_task::current().id().as_u64() as c_int)
    })
}

/// Exit current task
#[track_caller]
pub fn sys_exit(exit_code: c_int) -> ! {
    debug!("sys_exit <= {exit_code}");
    ax_task::exit(exit_code);
}
