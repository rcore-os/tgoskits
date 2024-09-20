use arceos_posix_api as api;

pub(crate) fn sys_sched_yield() -> i32 {
    api::sys_sched_yield()
}

pub(crate) fn sys_nanosleep(
    req: *const api::ctypes::timespec,
    rem: *mut api::ctypes::timespec,
) -> i32 {
    unsafe { api::sys_nanosleep(req, rem) }
}
