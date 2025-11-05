use axerrno::{AxError, AxResult};
use axhal::time::{TimeValue, monotonic_time, monotonic_time_nanos, nanos_to_ticks, wall_time};
use axtask::current;
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE,
    CLOCK_MONOTONIC_RAW, CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE,
    CLOCK_THREAD_CPUTIME_ID, itimerval, timespec, timeval,
};
use starry_core::{task::AsThread, time::ITimerType};
use starry_vm::{VmMutPtr, VmPtr};

use crate::time::TimeValueLike;

pub fn sys_clock_gettime(clock_id: __kernel_clockid_t, ts: *mut timespec) -> AxResult<isize> {
    let now = match clock_id as u32 {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => wall_time(),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => {
            monotonic_time()
        }
        CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => {
            let (utime, stime) = current().as_thread().time.borrow().output();
            utime + stime
        }
        _ => {
            warn!("Called sys_clock_gettime for unsupported clock {clock_id}");
            wall_time()
            // return Err(AxError::EINVAL);
        }
    };
    ts.vm_write(timespec::from_time_value(now))?;
    Ok(0)
}

pub fn sys_gettimeofday(ts: *mut timeval) -> AxResult<isize> {
    ts.vm_write(timeval::from_time_value(wall_time()))?;
    Ok(0)
}

pub fn sys_clock_getres(clock_id: __kernel_clockid_t, res: *mut timespec) -> AxResult<isize> {
    if clock_id as u32 != CLOCK_MONOTONIC && clock_id as u32 != CLOCK_REALTIME {
        warn!("Called sys_clock_getres for unsupported clock {clock_id}");
    }
    if let Some(res) = res.nullable() {
        res.vm_write(timespec::from_time_value(TimeValue::from_micros(1)))?;
    }
    Ok(0)
}

#[repr(C)]
pub struct Tms {
    /// user time
    tms_utime: usize,
    /// system time
    tms_stime: usize,
    /// user time of children
    tms_cutime: usize,
    /// system time of children
    tms_cstime: usize,
}

pub fn sys_times(tms: *mut Tms) -> AxResult<isize> {
    let (utime, stime) = current().as_thread().time.borrow().output();
    let utime = utime.as_micros() as usize;
    let stime = stime.as_micros() as usize;
    tms.vm_write(Tms {
        tms_utime: utime,
        tms_stime: stime,
        tms_cutime: utime,
        tms_cstime: stime,
    })?;
    Ok(nanos_to_ticks(monotonic_time_nanos()) as _)
}

pub fn sys_getitimer(which: i32, value: *mut itimerval) -> AxResult<isize> {
    let ty = ITimerType::from_repr(which).ok_or(AxError::InvalidInput)?;
    let (it_interval, it_value) = current().as_thread().time.borrow().get_itimer(ty);

    value.vm_write(itimerval {
        it_interval: timeval::from_time_value(it_interval),
        it_value: timeval::from_time_value(it_value),
    })?;
    Ok(0)
}

pub fn sys_setitimer(
    which: i32,
    new_value: *const itimerval,
    old_value: *mut itimerval,
) -> AxResult<isize> {
    let ty = ITimerType::from_repr(which).ok_or(AxError::InvalidInput)?;
    let curr = current();

    let (interval, remained) = match new_value.nullable() {
        Some(new_value) => {
            // FIXME: AnyBitPattern
            let new_value = unsafe { new_value.vm_read_uninit()?.assume_init() };
            (
                new_value.it_interval.try_into_time_value()?.as_nanos() as usize,
                new_value.it_value.try_into_time_value()?.as_nanos() as usize,
            )
        }
        None => (0, 0),
    };

    debug!("sys_setitimer <= type: {ty:?}, interval: {interval:?}, remained: {remained:?}");

    let old = curr
        .as_thread()
        .time
        .borrow_mut()
        .set_itimer(ty, interval, remained);

    if let Some(old_value) = old_value.nullable() {
        old_value.vm_write(itimerval {
            it_interval: timeval::from_time_value(old.0),
            it_value: timeval::from_time_value(old.1),
        })?;
    }
    Ok(0)
}
