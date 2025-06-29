use axerrno::{LinuxError, LinuxResult};
use axhal::time::TimeValue;
use axtask::{AxCpuMask, current};
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_MONOTONIC, CLOCK_REALTIME, TIMER_ABSTIME, timespec,
};

use crate::{
    ptr::{UserConstPtr, UserPtr, nullable},
    signal::have_signals,
    time::TimeValueLike,
};

pub fn sys_sched_yield() -> LinuxResult<isize> {
    axtask::yield_now();
    Ok(0)
}

fn sleep_impl(clock: impl Fn() -> TimeValue, dur: TimeValue) -> TimeValue {
    debug!("sleep_impl <= {:?}", dur);
    let start = clock();

    while clock() < start + dur {
        if have_signals() {
            break;
        }
        axtask::yield_now();
    }

    clock() - start
}

/// Sleep some nanoseconds
pub fn sys_nanosleep(req: UserConstPtr<timespec>, rem: UserPtr<timespec>) -> LinuxResult<isize> {
    let req = req.get_as_ref()?;

    let dur = req.try_into_time_value()?;
    debug!("sys_nanosleep <= req: {:?}", dur);

    let actual = sleep_impl(axhal::time::monotonic_time, dur);

    if let Some(diff) = dur.checked_sub(actual) {
        debug!("sys_nanosleep => rem: {:?}", diff);
        if let Some(rem) = nullable!(rem.get_as_mut())? {
            *rem = timespec::from_time_value(diff);
        }
        Err(LinuxError::EINTR)
    } else {
        Ok(0)
    }
}

pub fn sys_clock_nanosleep(
    clock_id: __kernel_clockid_t,
    flags: u32,
    req: UserConstPtr<timespec>,
    rem: UserPtr<timespec>,
) -> LinuxResult<isize> {
    let clock = match clock_id as u32 {
        CLOCK_REALTIME => axhal::time::wall_time,
        CLOCK_MONOTONIC => axhal::time::monotonic_time,
        _ => {
            warn!("Unsupported clock_id: {}", clock_id);
            return Err(LinuxError::EINVAL);
        }
    };

    let req = req.get_as_ref()?.try_into_time_value()?;
    debug!(
        "sys_clock_nanosleep <= clock_id: {}, flags: {}, req: {:?}",
        clock_id, flags, req
    );

    let dur = if flags & TIMER_ABSTIME != 0 {
        req.saturating_sub(clock())
    } else {
        req
    };

    let actual = sleep_impl(clock, dur);

    if let Some(diff) = dur.checked_sub(actual) {
        debug!("sys_clock_nanosleep => rem: {:?}", diff);
        if let Some(rem) = nullable!(rem.get_as_mut())? {
            *rem = timespec::from_time_value(diff);
        }
        Err(LinuxError::EINTR)
    } else {
        Ok(0)
    }
}

pub fn sys_sched_getaffinity(
    pid: i32,
    cpusetsize: usize,
    user_mask: UserPtr<u8>,
) -> LinuxResult<isize> {
    if cpusetsize * 8 < axconfig::SMP {
        return Err(LinuxError::EINVAL);
    }

    // TODO: support other threads
    if pid != 0 {
        return Err(LinuxError::EPERM);
    }

    let mask = current().cpumask();
    let mask_bytes = mask.as_bytes();
    user_mask
        .get_as_mut_slice(mask_bytes.len())?
        .copy_from_slice(mask_bytes);

    Ok(0)
}

pub fn sys_sched_setaffinity(
    pid: i32,
    cpusetsize: usize,
    user_mask: UserConstPtr<u8>,
) -> LinuxResult<isize> {
    let size = cpusetsize.min(axconfig::SMP.div_ceil(8));
    let user_mask = user_mask.get_as_slice(size)?;
    let mut cpu_mask = AxCpuMask::new();

    for i in 0..(size * 8).min(axconfig::SMP) {
        if user_mask[i / 8] & (1 << (i % 8)) != 0 {
            cpu_mask.set(i, true);
        }
    }

    // TODO: support other threads
    if pid != 0 {
        return Err(LinuxError::EPERM);
    }
    axtask::set_current_affinity(cpu_mask);

    Ok(0)
}
