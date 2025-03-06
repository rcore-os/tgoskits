use core::ffi::c_int;

use arceos_posix_api::{self as api, ctypes::timeval};
use axhal::time::{monotonic_time_nanos, nanos_to_ticks};

use crate::{
    ctypes::Tms,
    ptr::{PtrWrapper, UserPtr},
    syscall_body,
    task::time_stat_output,
};

pub(crate) fn sys_clock_gettime(clock_id: i32, tp: UserPtr<api::ctypes::timespec>) -> i32 {
    syscall_body!(sys_clock_gettime, unsafe {
        Ok(api::sys_clock_gettime(clock_id, tp.get()?))
    })
}

pub(crate) fn sys_get_time_of_day(ts: UserPtr<timeval>) -> c_int {
    syscall_body!(sys_get_time_of_day, unsafe {
        Ok(api::sys_get_time_of_day(ts.get()?))
    })
}

pub fn sys_times(tms: UserPtr<Tms>) -> isize {
    syscall_body!(sys_times, {
        let (_, utime_us, _, stime_us) = time_stat_output();
        unsafe {
            *tms.get()? = Tms {
                tms_utime: utime_us,
                tms_stime: stime_us,
                tms_cutime: utime_us,
                tms_cstime: stime_us,
            }
        }
        Ok(nanos_to_ticks(monotonic_time_nanos()) as isize)
    })
}
