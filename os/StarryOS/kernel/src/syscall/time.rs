use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::{
    NANOS_PER_SEC, TimeValue, monotonic_time, monotonic_time_nanos, nanos_to_ticks, wall_time,
};
use ax_task::current;
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE,
    CLOCK_MONOTONIC_RAW, CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE,
    CLOCK_THREAD_CPUTIME_ID, itimerval, timespec, timeval,
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    task::{AsThread, ITimerType, posix_timer::TimerSpec},
    time::TimeValueLike,
};

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
            return Err(AxError::InvalidInput);
        }
    };
    ts.vm_write(timespec::from_time_value(now))?;
    Ok(0)
}

#[derive(Default)]
#[repr(C)]
pub struct Timezone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}

pub fn sys_gettimeofday(ts: *mut timeval, tz: *mut Timezone) -> AxResult<isize> {
    if let Some(ts) = ts.nullable() {
        ts.vm_write(timeval::from_time_value(wall_time()))?;
    }
    if let Some(tz) = tz.nullable() {
        tz.vm_write(Timezone::default())?;
    }
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_time(tloc: *mut usize) -> AxResult<isize> {
    let secs = wall_time().as_secs() as isize;
    if let Some(tloc) = tloc.nullable() {
        tloc.vm_write(secs as usize)?;
    }
    Ok(secs)
}

pub fn sys_clock_getres(clock_id: __kernel_clockid_t, res: *mut timespec) -> AxResult<isize> {
    let resolution = match clock_id as u32 {
        CLOCK_REALTIME
        | CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_RAW
        | CLOCK_BOOTTIME
        | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID => TimeValue::from_nanos(1),
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => TimeValue::from_millis(4),
        _ => return Err(AxError::InvalidInput),
    };
    if let Some(res) = res.nullable() {
        res.vm_write(timespec::from_time_value(resolution))?;
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
    let (cutime, cstime) = current().as_thread().proc_data.children_cpu_time();
    tms.vm_write(Tms {
        tms_utime: utime.as_micros() as usize,
        tms_stime: stime.as_micros() as usize,
        tms_cutime: cutime.as_micros() as usize,
        tms_cstime: cstime.as_micros() as usize,
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

// ---- POSIX timer syscalls ----

use linux_raw_sys::general::{
    __kernel_itimerspec, __kernel_timer_t, __kernel_timespec, SIGEV_SIGNAL, sigevent,
};

pub fn sys_timer_create(
    clock_id: u32,
    sevp: *const sigevent,
    timerid: *mut __kernel_timer_t,
) -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();

    // Parse sigevent
    let (notify, signo, sival) = if let Some(sevp) = sevp.nullable() {
        let sev = unsafe { sevp.vm_read_uninit()?.assume_init() };
        // sigev_value is a union sigval { sival_int: i32, sival_ptr: *mut void }
        // On Linux, the kernel stores it as a pointer-sized field.
        let val = unsafe { sev.sigev_value.sival_ptr as i64 };
        (sev.sigev_notify as u32, sev.sigev_signo, val)
    } else {
        // NULL sevp defaults to SIGEV_SIGNAL with SIGALRM
        (SIGEV_SIGNAL, 14, 0i64) // SIGALRM = 14
    };

    let id = thr
        .proc_data
        .posix_timers
        .create(clock_id, notify, signo, sival)?;

    if let Err(e) = timerid.vm_write(id) {
        thr.proc_data.posix_timers.delete(id);
        return Err(e.into());
    }
    Ok(0)
}

pub fn sys_timer_settime(
    timerid: __kernel_timer_t,
    flags: i32,
    new_value: *const __kernel_itimerspec,
    old_value: *mut __kernel_itimerspec,
) -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();

    let new = unsafe { new_value.vm_read_uninit()?.assume_init() };

    let (old_interval, old_remaining) = thr
        .proc_data
        .posix_timers
        .settime(
            thr.proc_data.proc.pid(),
            timerid,
            flags,
            TimerSpec {
                value_sec: new.it_value.tv_sec,
                value_nsec: new.it_value.tv_nsec,
                interval_sec: new.it_interval.tv_sec,
                interval_nsec: new.it_interval.tv_nsec,
            },
        )
        .map_err(|_| AxError::InvalidInput)?;

    if let Some(old_value) = old_value.nullable() {
        let old_iv_sec = (old_interval / NANOS_PER_SEC) as i64;
        let old_iv_nsec = (old_interval % NANOS_PER_SEC) as i64;
        let old_rem_sec = (old_remaining / NANOS_PER_SEC) as i64;
        let old_rem_nsec = (old_remaining % NANOS_PER_SEC) as i64;
        old_value.vm_write(__kernel_itimerspec {
            it_interval: __kernel_timespec {
                tv_sec: old_iv_sec,
                tv_nsec: old_iv_nsec,
            },
            it_value: __kernel_timespec {
                tv_sec: old_rem_sec,
                tv_nsec: old_rem_nsec,
            },
        })?;
    }

    Ok(0)
}

pub fn sys_timer_gettime(
    timerid: __kernel_timer_t,
    curr_value: *mut __kernel_itimerspec,
) -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();

    let (interval, remaining) = thr
        .proc_data
        .posix_timers
        .gettime(timerid)
        .map_err(|_| AxError::InvalidInput)?;

    let iv_sec = (interval / NANOS_PER_SEC) as i64;
    let iv_nsec = (interval % NANOS_PER_SEC) as i64;
    let rem_sec = (remaining / NANOS_PER_SEC) as i64;
    let rem_nsec = (remaining % NANOS_PER_SEC) as i64;

    curr_value.vm_write(__kernel_itimerspec {
        it_interval: __kernel_timespec {
            tv_sec: iv_sec,
            tv_nsec: iv_nsec,
        },
        it_value: __kernel_timespec {
            tv_sec: rem_sec,
            tv_nsec: rem_nsec,
        },
    })?;

    Ok(0)
}

pub fn sys_timer_delete(timerid: __kernel_timer_t) -> AxResult<isize> {
    let curr = current();
    let thr = curr.as_thread();

    if thr.proc_data.posix_timers.delete(timerid) {
        Ok(0)
    } else {
        Err(AxError::InvalidInput)
    }
}
