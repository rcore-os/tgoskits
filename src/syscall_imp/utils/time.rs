use core::ffi::c_int;

use arceos_posix_api::{self as api, ctypes::timeval};
use axhal::time::{monotonic_time_nanos, nanos_to_ticks};

use crate::{ctypes::Tms, syscall_body, task::time_stat_output};

pub(crate) fn sys_clock_gettime(clock_id: i32, tp: *mut api::ctypes::timespec) -> i32 {
    unsafe { api::sys_clock_gettime(clock_id, tp) }
}

pub(crate) fn sys_get_time_of_day(ts: *mut timeval) -> c_int {
    unsafe { api::sys_get_time_of_day(ts) }
}

pub fn sys_times(tms: *mut Tms) -> isize {
    syscall_body!(sys_times, {
        let (_, utime_us, _, stime_us) = time_stat_output();
        unsafe {
            *tms = Tms {
                tms_utime: utime_us,
                tms_stime: stime_us,
                tms_cutime: utime_us,
                tms_cstime: stime_us,
            }
        }
        Ok(nanos_to_ticks(monotonic_time_nanos()) as isize)
    })
}
