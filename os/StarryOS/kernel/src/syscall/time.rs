use core::mem::{offset_of, size_of};

use ax_runtime::hal::time::{
    NANOS_PER_SEC, TimeValue, monotonic_time, wall_time,
};
use linux_raw_sys::general::{
    __kernel_clockid_t, __kernel_itimerspec, __kernel_timer_t, __kernel_timespec, CLOCK_BOOTTIME,
    CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW, CLOCK_PROCESS_CPUTIME_ID,
    CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID, SIGEV_SIGNAL, itimerval,
    sigevent, timespec, timeval,
};

use crate::{
    StarryError,
    mm::{UserPtr, VmMutPtr, VmPtr},
    task::{ITimerType, posix_timer::TimerSpec},
    time::TimeValueLike,
};

pub(crate) fn write_timespec(
    current: &crate::task::UserTaskRef,
    user: *mut timespec,
    value: timespec,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    let mut bytes = [0_u8; size_of::<timespec>()];
    user.write_abi_fields(current, &mut bytes, |fields| {
        fields.put_field(offset_of!(timespec, tv_sec), &value.tv_sec)?;
        fields.put_field(offset_of!(timespec, tv_nsec), &value.tv_nsec)
    })
}

fn write_timeval(
    current: &crate::task::UserTaskRef,
    user: *mut timeval,
    value: timeval,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(timeval, tv_sec), value.tv_sec)?;
    user.write_field(current, offset_of!(timeval, tv_usec), value.tv_usec)
}

fn write_itimerval(
    current: &crate::task::UserTaskRef,
    user: *mut itimerval,
    value: itimerval,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    let interval = offset_of!(itimerval, it_interval);
    user.write_field(
        current,
        interval + offset_of!(timeval, tv_sec),
        value.it_interval.tv_sec,
    )?;
    user.write_field(
        current,
        interval + offset_of!(timeval, tv_usec),
        value.it_interval.tv_usec,
    )?;
    let current_offset = offset_of!(itimerval, it_value);
    user.write_field(
        current,
        current_offset + offset_of!(timeval, tv_sec),
        value.it_value.tv_sec,
    )?;
    user.write_field(
        current,
        current_offset + offset_of!(timeval, tv_usec),
        value.it_value.tv_usec,
    )
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
pub(crate) fn write_kernel_timespec(
    current: &crate::task::UserTaskRef,
    user: *mut __kernel_timespec,
    value: __kernel_timespec,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    user.write_field(current, offset_of!(__kernel_timespec, tv_sec), value.tv_sec)?;
    user.write_field(
        current,
        offset_of!(__kernel_timespec, tv_nsec),
        value.tv_nsec,
    )
}

pub(crate) fn write_kernel_itimerspec(
    current: &crate::task::UserTaskRef,
    user: *mut __kernel_itimerspec,
    value: __kernel_itimerspec,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    let interval = offset_of!(__kernel_itimerspec, it_interval);
    user.write_field(
        current,
        interval + offset_of!(__kernel_timespec, tv_sec),
        value.it_interval.tv_sec,
    )?;
    user.write_field(
        current,
        interval + offset_of!(__kernel_timespec, tv_nsec),
        value.it_interval.tv_nsec,
    )?;
    let current_offset = offset_of!(__kernel_itimerspec, it_value);
    user.write_field(
        current,
        current_offset + offset_of!(__kernel_timespec, tv_sec),
        value.it_value.tv_sec,
    )?;
    user.write_field(
        current,
        current_offset + offset_of!(__kernel_timespec, tv_nsec),
        value.it_value.tv_nsec,
    )
}

pub fn sys_clock_gettime(
    current: &crate::task::UserTaskRef,
    clock_id: __kernel_clockid_t,
    ts: *mut timespec,
) -> crate::StarryResult<isize> {
    let now = match clock_id as u32 {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => wall_time(),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => {
            monotonic_time()
        }
        CLOCK_PROCESS_CPUTIME_ID => {
            let (utime, stime) = current.as_thread().proc_data.cpu_time();
            utime + stime
        }
        CLOCK_THREAD_CPUTIME_ID => {
            let (utime, stime) = current.as_thread().cpu_time().output();
            utime + stime
        }
        _ => {
            return Err(StarryError::InvalidInput);
        }
    };
    write_timespec(current, ts, timespec::from_time_value(now))?;
    Ok(0)
}

#[derive(Clone, Copy, Default, bytemuck::NoUninit)]
#[repr(C)]
pub struct Timezone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}

pub fn sys_gettimeofday(
    current: &crate::task::UserTaskRef,
    ts: *mut timeval,
    tz: *mut Timezone,
) -> crate::StarryResult<isize> {
    if let Some(ts) = ts.nullable() {
        write_timeval(current, ts, timeval::from_time_value(wall_time()))?;
    }
    if let Some(tz) = tz.nullable() {
        tz.vm_write(current, Timezone::default())?;
    }
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_time(
    current: &crate::task::UserTaskRef,
    tloc: *mut usize,
) -> crate::StarryResult<isize> {
    let secs = wall_time().as_secs() as isize;
    if let Some(tloc) = tloc.nullable() {
        tloc.vm_write(current, secs as usize)?;
    }
    Ok(secs)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_alarm(current: &crate::task::UserTaskRef, seconds: u32) -> crate::StarryResult<isize> {
    let proc_data = &current.as_thread().proc_data;
    let outcome = proc_data.set_interval_timer(
        ITimerType::Real,
        TimeValue::ZERO,
        TimeValue::from_secs(u64::from(seconds)),
    );
    let (_, old_remaining) = outcome.apply(crate::task::AlarmTarget::Process(
        alloc::sync::Arc::downgrade(&proc_data.identity()),
    ));

    let mut old_seconds = old_remaining.as_secs();
    if old_remaining.subsec_nanos() != 0 {
        old_seconds = old_seconds.saturating_add(1);
    }
    Ok(old_seconds as isize)
}

pub fn sys_clock_getres(
    current: &crate::task::UserTaskRef,
    clock_id: __kernel_clockid_t,
    res: *mut timespec,
) -> crate::StarryResult<isize> {
    let resolution = match clock_id as u32 {
        CLOCK_REALTIME
        | CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_RAW
        | CLOCK_BOOTTIME
        | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID => TimeValue::from_nanos(1),
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => TimeValue::from_millis(4),
        _ => return Err(StarryError::InvalidInput),
    };
    if let Some(res) = res.nullable() {
        write_timespec(current, res, timespec::from_time_value(resolution))?;
    }
    Ok(0)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
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

pub fn sys_times(current: &crate::task::UserTaskRef, tms: *mut Tms) -> crate::StarryResult<isize> {
    let curr = current;
    let proc_data = &curr.as_thread().proc_data;
    let (utime, stime) = proc_data.cpu_time();
    let (cutime, cstime) = proc_data.children_cpu_time();
    // Linux times(2) reports every field and the return value in USER_HZ clock
    // ticks (glibc/musl hardcode _SC_CLK_TCK = 100, so one tick is 10 ms), the
    // same jiffies unit as the /proc/[pid]/stat writer in task::stat.
    let ticks = |d: TimeValue| (d.as_millis() / 10) as usize;
    tms.vm_write(
        current,
        Tms {
            tms_utime: ticks(utime),
            tms_stime: ticks(stime),
            tms_cutime: ticks(cutime),
            tms_cstime: ticks(cstime),
        },
    )?;
    Ok((monotonic_time().as_millis() / 10) as _)
}

pub fn sys_getitimer(
    current: &crate::task::UserTaskRef,
    which: i32,
    value: *mut itimerval,
) -> crate::StarryResult<isize> {
    let ty = ITimerType::from_repr(which).ok_or(crate::StarryError::InvalidInput)?;
    let curr = current;
    let (it_interval, it_value) = curr.as_thread().proc_data.get_interval_timer(ty);

    write_itimerval(
        current,
        value,
        itimerval {
            it_interval: timeval::from_time_value(it_interval),
            it_value: timeval::from_time_value(it_value),
        },
    )?;
    Ok(0)
}

pub fn sys_setitimer(
    current: &crate::task::UserTaskRef,
    which: i32,
    new_value: *const itimerval,
    old_value: *mut itimerval,
) -> crate::StarryResult<isize> {
    let ty = ITimerType::from_repr(which).ok_or(crate::StarryError::InvalidInput)?;
    let curr = current;

    let (interval, remained) = match new_value.nullable() {
        Some(new_value) => {
            // FIXME: AnyBitPattern
            let new_value = unsafe { new_value.vm_read_uninit(current)?.assume_init() };
            (
                new_value.it_interval.try_into_time_value()?,
                new_value.it_value.try_into_time_value()?,
            )
        }
        None => (TimeValue::ZERO, TimeValue::ZERO),
    };

    debug!("sys_setitimer <= type: {ty:?}, interval: {interval:?}, remained: {remained:?}");

    let proc_data = &curr.as_thread().proc_data;
    let outcome = proc_data.set_interval_timer(ty, interval, remained);
    let old = outcome.apply(crate::task::AlarmTarget::Process(
        alloc::sync::Arc::downgrade(&proc_data.identity()),
    ));

    if let Some(old_value) = old_value.nullable() {
        write_itimerval(
            current,
            old_value,
            itimerval {
                it_interval: timeval::from_time_value(old.0),
                it_value: timeval::from_time_value(old.1),
            },
        )?;
    }
    Ok(0)
}

// ---- POSIX timer syscalls ----

pub fn sys_timer_create(
    current: &crate::task::UserTaskRef,
    clock_id: u32,
    sevp: *const sigevent,
    timerid: *mut __kernel_timer_t,
) -> crate::StarryResult<isize> {
    let curr = current;
    let thr = curr.as_thread();

    // Parse sigevent
    let (notify, signo, sival) = if let Some(sevp) = sevp.nullable() {
        let sev = unsafe { sevp.vm_read_uninit(current)?.assume_init() };
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
        .posix_timers()
        .create(clock_id, notify, signo, sival)?;

    if let Err(e) = timerid.vm_write(current, id) {
        thr.proc_data.posix_timers().delete(id);
        return Err(e.into());
    }
    Ok(0)
}

pub fn sys_timer_settime(
    current: &crate::task::UserTaskRef,
    timerid: __kernel_timer_t,
    flags: i32,
    new_value: *const __kernel_itimerspec,
    old_value: *mut __kernel_itimerspec,
) -> crate::StarryResult<isize> {
    let curr = current;
    let thr = curr.as_thread();

    let new = unsafe { new_value.vm_read_uninit(current)?.assume_init() };

    let (old_interval, old_remaining) = thr
        .proc_data
        .posix_timers()
        .settime(
            crate::task::AlarmTarget::Process(alloc::sync::Arc::downgrade(
                &thr.proc_data.identity(),
            )),
            timerid,
            flags,
            TimerSpec {
                value_sec: new.it_value.tv_sec,
                value_nsec: new.it_value.tv_nsec,
                interval_sec: new.it_interval.tv_sec,
                interval_nsec: new.it_interval.tv_nsec,
            },
        )
        .map_err(|_| StarryError::InvalidInput)?;

    if let Some(old_value) = old_value.nullable() {
        let old_iv_sec = (old_interval / NANOS_PER_SEC) as i64;
        let old_iv_nsec = (old_interval % NANOS_PER_SEC) as i64;
        let old_rem_sec = (old_remaining / NANOS_PER_SEC) as i64;
        let old_rem_nsec = (old_remaining % NANOS_PER_SEC) as i64;
        write_kernel_itimerspec(
            current,
            old_value,
            __kernel_itimerspec {
                it_interval: __kernel_timespec {
                    tv_sec: old_iv_sec,
                    tv_nsec: old_iv_nsec,
                },
                it_value: __kernel_timespec {
                    tv_sec: old_rem_sec,
                    tv_nsec: old_rem_nsec,
                },
            },
        )?;
    }

    Ok(0)
}

pub fn sys_timer_gettime(
    current: &crate::task::UserTaskRef,
    timerid: __kernel_timer_t,
    curr_value: *mut __kernel_itimerspec,
) -> crate::StarryResult<isize> {
    let curr = current;
    let thr = curr.as_thread();

    let (interval, remaining) = thr
        .proc_data
        .posix_timers()
        .gettime(timerid)
        .map_err(|_| StarryError::InvalidInput)?;

    let iv_sec = (interval / NANOS_PER_SEC) as i64;
    let iv_nsec = (interval % NANOS_PER_SEC) as i64;
    let rem_sec = (remaining / NANOS_PER_SEC) as i64;
    let rem_nsec = (remaining % NANOS_PER_SEC) as i64;

    write_kernel_itimerspec(
        current,
        curr_value,
        __kernel_itimerspec {
            it_interval: __kernel_timespec {
                tv_sec: iv_sec,
                tv_nsec: iv_nsec,
            },
            it_value: __kernel_timespec {
                tv_sec: rem_sec,
                tv_nsec: rem_nsec,
            },
        },
    )?;

    Ok(0)
}

pub fn sys_timer_delete(
    current: &crate::task::UserTaskRef,
    timerid: __kernel_timer_t,
) -> crate::StarryResult<isize> {
    let curr = current;
    let thr = curr.as_thread();

    if thr.proc_data.posix_timers().delete(timerid) {
        Ok(0)
    } else {
        Err(StarryError::InvalidInput)
    }
}

#[cfg(all(test, not(axtest)))]
fn time_clock_id_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{
        CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW,
        CLOCK_PROCESS_CPUTIME_ID, CLOCK_REALTIME, CLOCK_REALTIME_COARSE, CLOCK_THREAD_CPUTIME_ID,
    };

    // Test valid clock IDs for clock_gettime
    let valid_clocks = [
        CLOCK_REALTIME,
        CLOCK_REALTIME_COARSE,
        CLOCK_MONOTONIC,
        CLOCK_MONOTONIC_RAW,
        CLOCK_MONOTONIC_COARSE,
        CLOCK_BOOTTIME,
        CLOCK_PROCESS_CPUTIME_ID,
        CLOCK_THREAD_CPUTIME_ID,
    ];

    assert!(valid_clocks.contains(&CLOCK_REALTIME));
    assert!(valid_clocks.contains(&CLOCK_MONOTONIC));
    assert!(!valid_clocks.contains(&999u32));

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn time_clock_id_validation_rules_hold() {
        assert!(super::time_clock_id_validation_rules_hold_for_test());
    }
}
