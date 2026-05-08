use ax_errno::{AxError, AxResult};
use ax_hal::time::TimeValue;
use ax_task::{
    AxCpuMask, current,
    future::{block_on, interruptible, sleep},
};
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_MONOTONIC, CLOCK_REALTIME, PRIO_PGRP, PRIO_PROCESS, PRIO_USER,
    SCHED_RR, TIMER_ABSTIME, timespec,
};
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use crate::{
    task::{AsThread, ProcessData, get_process_data, get_process_group, get_task, processes},
    time::TimeValueLike,
};

pub fn sys_sched_yield() -> AxResult<isize> {
    ax_task::yield_now();
    Ok(0)
}

fn sleep_impl(clock: impl Fn() -> TimeValue, dur: TimeValue) -> TimeValue {
    debug!("sleep_impl <= {dur:?}");

    let start = clock();

    // TODO: currently ignoring concrete clock type
    // We detect EINTR manually if the slept time is not enough.
    let _ = block_on(interruptible(sleep(dur)));

    clock() - start
}

/// Sleep some nanoseconds
pub fn sys_nanosleep(req: *const timespec, rem: *mut timespec) -> AxResult<isize> {
    // FIXME: AnyBitPattern
    let req = unsafe { req.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    debug!("sys_nanosleep <= req: {req:?}");

    let actual = sleep_impl(ax_hal::time::monotonic_time, req);

    if let Some(diff) = req.checked_sub(actual) {
        debug!("sys_nanosleep => rem: {diff:?}");
        if let Some(rem) = rem.nullable() {
            rem.vm_write(timespec::from_time_value(diff))?;
        }
        Err(AxError::Interrupted)
    } else {
        Ok(0)
    }
}

pub fn sys_clock_nanosleep(
    clock_id: __kernel_clockid_t,
    flags: u32,
    req: *const timespec,
    rem: *mut timespec,
) -> AxResult<isize> {
    let clock = match clock_id as u32 {
        CLOCK_REALTIME => ax_hal::time::wall_time,
        CLOCK_MONOTONIC => ax_hal::time::monotonic_time,
        _ => {
            warn!("Unsupported clock_id: {clock_id}");
            return Err(AxError::InvalidInput);
        }
    };

    let req = unsafe { req.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    debug!("sys_clock_nanosleep <= clock_id: {clock_id}, flags: {flags}, req: {req:?}");

    let dur = if flags & TIMER_ABSTIME != 0 {
        req.saturating_sub(clock())
    } else {
        req
    };

    let actual = sleep_impl(clock, dur);

    if let Some(diff) = dur.checked_sub(actual) {
        debug!("sys_clock_nanosleep => rem: {diff:?}");
        if let Some(rem) = rem.nullable() {
            rem.vm_write(timespec::from_time_value(diff))?;
        }
        Err(AxError::Interrupted)
    } else {
        Ok(0)
    }
}

pub fn sys_sched_getaffinity(pid: i32, cpusetsize: usize, user_mask: *mut u8) -> AxResult<isize> {
    if cpusetsize * 8 < ax_hal::cpu_num() {
        return Err(AxError::InvalidInput);
    }

    let task = get_task_by_sched_pid(pid)?;
    let mask = task.cpumask();
    let mask_bytes = mask.as_bytes();

    vm_write_slice(user_mask, mask_bytes)?;

    Ok(mask_bytes.len() as _)
}

pub fn sys_sched_setaffinity(pid: i32, cpusetsize: usize, user_mask: *const u8) -> AxResult<isize> {
    let size = cpusetsize.min(ax_hal::cpu_num().div_ceil(8));
    let user_mask = vm_load(user_mask, size)?;
    let mut cpu_mask = AxCpuMask::new();

    for i in 0..(size * 8).min(ax_hal::cpu_num()) {
        if user_mask[i / 8] & (1 << (i % 8)) != 0 {
            cpu_mask.set(i, true);
        }
    }

    if cpu_mask.is_empty() {
        return Err(AxError::InvalidInput);
    }

    let task = get_task_by_sched_pid(pid)?;
    if task.id() == current().id() {
        ax_task::set_current_affinity(cpu_mask);
    } else {
        task.set_cpumask(cpu_mask);
        task.interrupt();
    }

    Ok(0)
}

fn get_task_by_sched_pid(pid: i32) -> AxResult<ax_task::AxTaskRef> {
    if pid < 0 {
        return Err(AxError::InvalidInput);
    }
    get_task(pid as _)
}

pub fn sys_sched_getscheduler(_pid: i32) -> AxResult<isize> {
    Ok(SCHED_RR as _)
}

pub fn sys_sched_setscheduler(_pid: i32, _policy: i32, _param: *const ()) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_sched_getparam(_pid: i32, _param: *mut ()) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_getpriority(which: u32, who: u32) -> AxResult<isize> {
    debug!("sys_getpriority <= which: {which}, who: {who}");

    match which {
        PRIO_PROCESS => {
            let proc = get_process_data(who)?;
            Ok(raw_priority(proc.nice()))
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current().as_thread().proc_data.proc.group().pgid()
            } else {
                get_process_group(who)?.pgid()
            };
            min_priority_for_processes(
                processes()
                    .into_iter()
                    .filter(|proc| proc.proc.group().pgid() == pgid),
            )
        }
        PRIO_USER => {
            if who == 0 {
                Ok(raw_priority(current().as_thread().proc_data.nice()))
            } else {
                Err(AxError::NoSuchProcess)
            }
        }
        _ => Err(AxError::InvalidInput),
    }
}

pub fn sys_setpriority(which: u32, who: u32, prio: i32) -> AxResult<isize> {
    debug!("sys_setpriority <= which: {which}, who: {who}, prio: {prio}");

    let nice = prio.clamp(-20, 19);
    match which {
        PRIO_PROCESS => {
            let proc = get_process_data(who)?;
            proc.set_nice(nice);
            Ok(0)
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current().as_thread().proc_data.proc.group().pgid()
            } else {
                get_process_group(who)?.pgid()
            };
            set_priority_for_processes(
                processes()
                    .into_iter()
                    .filter(|proc| proc.proc.group().pgid() == pgid),
                nice,
            )
        }
        PRIO_USER => {
            if who == 0 {
                current().as_thread().proc_data.set_nice(nice);
                Ok(0)
            } else {
                Err(AxError::NoSuchProcess)
            }
        }
        _ => Err(AxError::InvalidInput),
    }
}

fn raw_priority(nice: i32) -> isize {
    (20 - nice) as isize
}

fn min_priority_for_processes(
    procs: impl Iterator<Item = alloc::sync::Arc<ProcessData>>,
) -> AxResult<isize> {
    procs
        .map(|proc| proc.nice())
        .min()
        .map(raw_priority)
        .ok_or(AxError::NoSuchProcess)
}

fn set_priority_for_processes(
    procs: impl Iterator<Item = alloc::sync::Arc<ProcessData>>,
    nice: i32,
) -> AxResult<isize> {
    let mut found = false;
    for proc in procs {
        proc.set_nice(nice);
        found = true;
    }
    if found {
        Ok(0)
    } else {
        Err(AxError::NoSuchProcess)
    }
}
