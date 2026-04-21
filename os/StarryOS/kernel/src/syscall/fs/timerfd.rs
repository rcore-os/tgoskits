use ax_errno::{AxError, AxResult};
use linux_raw_sys::general::{__kernel_clockid_t, TFD_CLOEXEC, TFD_NONBLOCK, itimerspec};
use starry_vm::{VmMutPtr, VmPtr};

use crate::file::{FileLike, add_file_like, timerfd::TimerFd};

pub fn sys_timerfd_create(clock_id: __kernel_clockid_t, flags: u32) -> AxResult<isize> {
    debug!("sys_timerfd_create <= clock_id: {clock_id}, flags: {flags:#x}");
    if flags & !(TFD_CLOEXEC | TFD_NONBLOCK) != 0 {
        return Err(AxError::InvalidInput);
    }

    let timerfd = TimerFd::new(clock_id);
    timerfd.set_nonblocking(flags & TFD_NONBLOCK != 0)?;
    add_file_like(timerfd as _, flags & TFD_CLOEXEC != 0).map(|fd| fd as isize)
}

pub fn sys_timerfd_settime(
    fd: i32,
    flags: u32,
    new_value: *const itimerspec,
    old_value: *mut itimerspec,
) -> AxResult<isize> {
    debug!("sys_timerfd_settime <= fd: {fd}, flags: {flags:#x}");
    let timerfd = TimerFd::from_fd(fd)?;
    let new_value = unsafe { new_value.vm_read_uninit()?.assume_init() };
    let old = timerfd.set_time(flags, new_value)?;
    if let Some(old_value) = old_value.nullable() {
        old_value.vm_write(old)?;
    }
    Ok(0)
}

pub fn sys_timerfd_gettime(fd: i32, curr_value: *mut itimerspec) -> AxResult<isize> {
    debug!("sys_timerfd_gettime <= fd: {fd}");
    let timerfd = TimerFd::from_fd(fd)?;
    curr_value.vm_write(timerfd.get_time()?)?;
    Ok(0)
}
