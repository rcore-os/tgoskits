use axerrno::{LinuxError, LinuxResult};
use axtask::{AxCpuMask, current};
use linux_raw_sys::general::timespec;

use crate::{
    ptr::{UserConstPtr, UserPtr, nullable},
    signal::have_signals,
    time::TimeValueLike,
};

pub fn sys_sched_yield() -> LinuxResult<isize> {
    axtask::yield_now();
    Ok(0)
}

/// Sleep some nanoseconds
///
/// TODO: should be woken by signals, and set errno
pub fn sys_nanosleep(req: UserConstPtr<timespec>, rem: UserPtr<timespec>) -> LinuxResult<isize> {
    let req = req.get_as_ref()?;

    if req.tv_nsec < 0 || req.tv_nsec > 999_999_999 || req.tv_sec < 0 {
        return Err(LinuxError::EINVAL);
    }

    let dur = req.to_time_value();
    debug!("sys_nanosleep <= {:?}", dur);

    let now = axhal::time::monotonic_time();

    while axhal::time::monotonic_time() < now + dur {
        if have_signals() {
            break;
        }
        axtask::yield_now();
    }

    let after = axhal::time::monotonic_time();
    let actual = after - now;

    if let Some(diff) = dur.checked_sub(actual) {
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
