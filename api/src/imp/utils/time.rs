use arceos_posix_api::{self as api, ctypes::timeval};
use axerrno::LinuxResult;
use axhal::time::{monotonic_time_nanos, nanos_to_ticks};
use starry_core::task::time_stat_output;

use crate::ptr::{PtrWrapper, UserPtr};

pub fn sys_clock_gettime(clock_id: i32, tp: UserPtr<api::ctypes::timespec>) -> LinuxResult<isize> {
    unsafe { Ok(api::sys_clock_gettime(clock_id, tp.get()?) as _) }
}

pub fn sys_get_time_of_day(ts: UserPtr<timeval>) -> LinuxResult<isize> {
    unsafe { Ok(api::sys_get_time_of_day(ts.get()?) as _) }
}

#[repr(C)]
pub struct Tms {
    /// 进程用户态执行时间，单位为us
    tms_utime: usize,
    /// 进程内核态执行时间，单位为us
    tms_stime: usize,
    /// 子进程用户态执行时间和，单位为us
    tms_cutime: usize,
    /// 子进程内核态执行时间和，单位为us
    tms_cstime: usize,
}

pub fn sys_times(tms: UserPtr<Tms>) -> LinuxResult<isize> {
    let (_, utime_us, _, stime_us) = time_stat_output();
    unsafe {
        *tms.get()? = Tms {
            tms_utime: utime_us,
            tms_stime: stime_us,
            tms_cutime: utime_us,
            tms_cstime: stime_us,
        }
    }
    Ok(nanos_to_ticks(monotonic_time_nanos()) as _)
}
