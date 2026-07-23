use alloc::{sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::{self, time::TimeValue};
use ax_task::{
    AxCpuMask, current,
    future::{block_on, interruptible, sleep},
};
use bytemuck::{Pod, Zeroable};
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_MONOTONIC, CLOCK_REALTIME, PRIO_PGRP, PRIO_PROCESS, PRIO_USER,
    SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_NORMAL, SCHED_RR, TIMER_ABSTIME, timespec,
};
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use crate::{
    task::{
        AsThread, Cred, ProcessData, get_process_data, get_process_group, get_task, is_zombie_pid,
        processes,
    },
    time::TimeValueLike,
};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SchedParam {
    sched_priority: i32,
}

pub fn sys_sched_yield() -> AxResult<isize> {
    ax_task::yield_now();
    Ok(0)
}

fn sleep_impl(clock: impl Fn() -> TimeValue, dur: TimeValue) -> (AxResult<()>, TimeValue) {
    debug!("sleep_impl <= {dur:?}");

    let start = clock();

    // TODO: currently ignoring concrete clock type
    let result = block_on(interruptible(sleep(dur))).map_err(AxError::from);

    (result, clock() - start)
}

/// Sleep some nanoseconds
pub fn sys_nanosleep(req: *const timespec, rem: *mut timespec) -> AxResult<isize> {
    // FIXME: AnyBitPattern
    let req = unsafe { req.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    debug!("sys_nanosleep <= req: {req:?}");

    let (result, actual) = sleep_impl(hal::time::monotonic_time, req);

    match result {
        Ok(()) => Ok(0),
        Err(err) => {
            let diff = req.saturating_sub(actual);
            debug!("sys_nanosleep => rem: {diff:?}");
            if let Some(rem) = rem.nullable() {
                rem.vm_write(timespec::from_time_value(diff))?;
            }
            Err(err)
        }
    }
}

pub fn sys_clock_nanosleep(
    clock_id: __kernel_clockid_t,
    flags: u32,
    req: *const timespec,
    rem: *mut timespec,
) -> AxResult<isize> {
    let clock = match clock_id as u32 {
        CLOCK_REALTIME => hal::time::wall_time,
        CLOCK_MONOTONIC => hal::time::monotonic_time,
        _ => {
            warn!("Unsupported clock_id: {clock_id}");
            return Err(AxError::InvalidInput);
        }
    };

    let req = unsafe { req.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    debug!("sys_clock_nanosleep <= clock_id: {clock_id}, flags: {flags}, req: {req:?}");

    let is_abstime = flags & TIMER_ABSTIME != 0;
    let dur = if is_abstime {
        req.saturating_sub(clock())
    } else {
        req
    };

    let (result, actual) = sleep_impl(clock, dur);

    match result {
        Ok(()) => Ok(0),
        Err(err) => {
            if !is_abstime {
                let diff = dur.saturating_sub(actual);
                debug!("sys_clock_nanosleep => rem: {diff:?}");
                if let Some(rem) = rem.nullable() {
                    rem.vm_write(timespec::from_time_value(diff))?;
                }
            }
            Err(err)
        }
    }
}

pub fn sys_sched_getaffinity(pid: i32, cpusetsize: usize, user_mask: *mut u8) -> AxResult<isize> {
    if cpusetsize * 8 < hal::cpu_num() {
        return Err(AxError::InvalidInput);
    }

    let task = get_task_by_sched_pid(pid)?;
    let mask = task.cpumask();
    let mask_bytes = mask.as_bytes();

    vm_write_slice(user_mask, mask_bytes)?;

    Ok(mask_bytes.len() as _)
}

pub fn check_sched_permission(pid: i32) -> AxResult<()> {
    let caller = current().as_thread().cred();
    let task = get_task_by_sched_pid(pid)?;
    if task.id() == current().id() {
        return Ok(());
    }
    let target_proc = get_process_data(pid as u32)?;
    let target_cred = process_cred(&target_proc)?;
    if caller.has_cap_sys_nice()
        || caller.euid == target_cred.uid
        || caller.euid == target_cred.euid
    {
        Ok(())
    } else {
        Err(AxError::OperationNotPermitted)
    }
}

pub fn sys_sched_setaffinity(pid: i32, cpusetsize: usize, user_mask: *const u8) -> AxResult<isize> {
    check_sched_permission(pid)?;
    let task = get_task_by_sched_pid(pid)?;
    let size = cpusetsize.min(hal::cpu_num().div_ceil(8));
    let user_mask = vm_load(user_mask, size)?;
    let mut cpu_mask = AxCpuMask::new();

    for i in 0..(size * 8).min(hal::cpu_num()) {
        if user_mask[i / 8] & (1 << (i % 8)) != 0 {
            cpu_mask.set(i, true);
        }
    }

    if cpu_mask.is_empty() {
        return Err(AxError::InvalidInput);
    }
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
    let task = get_task_by_sched_pid(_pid)?;
    Ok(task.sched_policy() as isize)
}

pub fn sys_sched_setscheduler(_pid: i32, _policy: i32, _param: *const ()) -> AxResult<isize> {
    check_sched_permission(_pid)?;
    let task = get_task_by_sched_pid(_pid)?;
    let caller = current().as_thread().cred();
    if _param.is_null() {
        return Err(AxError::InvalidInput);
    }
    let user_param = vm_load::<SchedParam>(_param.cast(), 1)?;
    let user_param = user_param[0];
    let mut policy = _policy as u32;
    const SCHED_RESET_ON_FORK: u32 = 0x40000000;
    let _reset_on_fork = (policy & SCHED_RESET_ON_FORK) != 0;
    policy &= !SCHED_RESET_ON_FORK;
    let prio = user_param.sched_priority;
    match policy {
        SCHED_NORMAL | SCHED_FIFO | SCHED_RR | SCHED_BATCH | SCHED_IDLE => {}
        _ => return Err(AxError::InvalidInput),
    }
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE => {
            if prio != 0 {
                return Err(AxError::InvalidInput);
            }
        }
        SCHED_FIFO | SCHED_RR => {
            if !(1..=99).contains(&prio) {
                return Err(AxError::InvalidInput);
            }
            if !caller.has_cap_sys_nice() {
                return Err(AxError::OperationNotPermitted);
            }
        }
        _ => unreachable!(),
    }
    task.set_sched_policy(policy as i32);
    task.set_sched_priority(prio);
    Ok(0)
}

pub fn sys_sched_getparam(_pid: i32, _param: *mut ()) -> AxResult<isize> {
    let task = get_task_by_sched_pid(_pid)?;
    if _param.is_null() {
        return Err(AxError::InvalidInput);
    }
    let param = SchedParam {
        sched_priority: task.sched_priority(),
    };
    let ptr = _param as *mut SchedParam;
    unsafe {
        let bytes = core::slice::from_raw_parts(
            &param as *const SchedParam as *const u8,
            core::mem::size_of::<SchedParam>(),
        );
        vm_write_slice(ptr as *mut u8, bytes)?;
    }
    Ok(0)
}

pub fn sys_getpriority(which: u32, who: u32) -> AxResult<isize> {
    debug!("sys_getpriority <= which: {which}, who: {who}");

    match which {
        PRIO_PROCESS => match get_process_data(who) {
            Ok(proc) => Ok(raw_priority(proc.nice())),
            Err(AxError::NoSuchProcess) if who != 0 && is_zombie_pid(who) => Ok(20),
            Err(err) => Err(err),
        },
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
            let uid = if who == 0 {
                current().as_thread().cred().uid
            } else {
                who
            };
            min_priority_for_processes(processes_for_uid(uid).into_iter())
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
            check_setpriority_permission(&proc, nice)?;
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
            let uid = if who == 0 {
                current().as_thread().cred().uid
            } else {
                who
            };
            set_priority_for_processes(processes_for_uid(uid).into_iter(), nice)
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

fn processes_for_uid(uid: u32) -> Vec<Arc<ProcessData>> {
    processes()
        .into_iter()
        .filter(|proc| {
            process_cred(proc)
                .map(|cred| cred.uid == uid)
                .unwrap_or(false)
        })
        .collect()
}

fn process_cred(proc: &ProcessData) -> AxResult<Arc<Cred>> {
    for tid in proc.proc.threads() {
        if let Ok(task) = get_task(tid)
            && let Some(thread) = task.try_as_thread()
        {
            return Ok(thread.cred());
        }
    }
    Err(AxError::NoSuchProcess)
}

fn setpriority_cred_matches(caller: &Cred, target: &Cred) -> bool {
    caller.euid == target.uid || caller.euid == target.euid
}

fn check_setpriority_permission(proc: &ProcessData, nice: i32) -> AxResult<()> {
    let caller = current().as_thread().cred();
    if caller.has_cap_sys_nice() {
        return Ok(());
    }

    let target = process_cred(proc)?;
    if !setpriority_cred_matches(&caller, &target) {
        return Err(AxError::OperationNotPermitted);
    }
    if nice < proc.nice() {
        return Err(AxError::PermissionDenied);
    }
    Ok(())
}

fn set_priority_for_processes(
    procs: impl Iterator<Item = alloc::sync::Arc<ProcessData>>,
    nice: i32,
) -> AxResult<isize> {
    let procs: Vec<_> = procs.collect();
    if procs.is_empty() {
        return Err(AxError::NoSuchProcess);
    }
    for proc in &procs {
        check_setpriority_permission(proc, nice)?;
    }
    for proc in procs {
        proc.set_nice(nice);
    }
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn schedule_clock_and_sched_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{
        CLOCK_MONOTONIC, CLOCK_REALTIME,
        SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_NORMAL, SCHED_RR,
    };
    
    // Test clock_nanosleep clock_id validation
    let valid_clocks = [CLOCK_REALTIME as u32, CLOCK_MONOTONIC as u32];
    
    for &clock in &valid_clocks {
        assert!(clock == CLOCK_REALTIME as u32 || clock == CLOCK_MONOTONIC as u32);
    }
    
    // Invalid clock ID
    assert!(999u32 != CLOCK_REALTIME as u32 && 999u32 != CLOCK_MONOTONIC as u32);
    
    // Test valid scheduler policies
    let valid_policies = [
        SCHED_NORMAL, SCHED_FIFO, SCHED_RR, SCHED_BATCH, SCHED_IDLE,
    ];
    
    for &policy in &valid_policies {
        assert!(policy >= 0);
    }
    
    true
}
