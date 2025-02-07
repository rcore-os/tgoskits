use arceos_posix_api::{
    self as api,
    ctypes::{clockid_t, timespec},
};
use axerrno::LinuxError;

pub(crate) fn sys_sched_yield() -> i32 {
    api::sys_sched_yield()
}

pub(crate) fn sys_nanosleep(req: *const timespec, rem: *mut timespec) -> i32 {
    unsafe { api::sys_nanosleep(req, rem) }
}

pub(crate) fn sys_clock_nanosleep(
    clock_id: clockid_t,
    flags: isize,
    req: *const timespec,
    rem: *mut timespec,
) -> i32 {
    // CLOCK defaults to CLOCK_REALTIME
    // flags defaults to 0

    if clock_id != api::ctypes::CLOCK_REALTIME as clockid_t {
        // For older linux headers, it does not define ENOTSUP, so we use EOPNOTSUPP instead
        return -LinuxError::EOPNOTSUPP.code();
    }

    if flags != 0 {
        return -LinuxError::EOPNOTSUPP.code();
    }

    unsafe { api::sys_nanosleep(req, rem) }
}
