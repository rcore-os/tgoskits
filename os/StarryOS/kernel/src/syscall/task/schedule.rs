use alloc::{sync::Arc, vec, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::{self, time::TimeValue};
use ax_std::os::arceos::task as scheduler;
use bytemuck::{Pod, Zeroable};
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
use linux_raw_sys::general::__kernel_timespec;
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_MONOTONIC, CLOCK_REALTIME, PRIO_PGRP, PRIO_PROCESS, PRIO_USER,
    RLIMIT_NICE, RLIMIT_RTPRIO, SCHED_RESET_ON_FORK, TIMER_ABSTIME, timespec,
};
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::schedule_abi::{
    SchedAttr, ScheduleUpdate, SchedulerPermission, check_policy_permission,
    check_reset_on_fork_permission, linux_policy_number, linux_sched_priority, parse_sched_attr,
    parse_setscheduler, sched_attr_from_policy, scheduler_priority_max, scheduler_priority_min,
};
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
use crate::syscall::time::write_kernel_timespec;
use crate::{
    syscall::time::write_timespec,
    task::{
        Cred, ProcessData, UserTaskRef, current_user_task,
        future::{block_on_user, interruptible_for, sleep},
        get_process_group, get_task, is_zombie_pid, processes,
    },
    time::TimeValueLike,
};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SchedParam {
    sched_priority: i32,
}

pub fn sys_sched_yield() -> AxResult<isize> {
    scheduler::yield_current_cpu().map_err(map_task_error)?;
    Ok(0)
}

pub fn sys_sched_get_priority_min(policy: i32) -> AxResult<isize> {
    let policy = u32::try_from(policy).map_err(|_| AxError::InvalidInput)?;
    Ok(scheduler_priority_min(policy)? as isize)
}

pub fn sys_sched_get_priority_max(policy: i32) -> AxResult<isize> {
    let policy = u32::try_from(policy).map_err(|_| AxError::InvalidInput)?;
    Ok(scheduler_priority_max(policy)? as isize)
}

pub fn sys_sched_rr_get_interval(pid: i32, user_interval: *mut timespec) -> AxResult<isize> {
    let interval = TimeValue::from_nanos(scheduler_interval_ns(pid)?);
    write_timespec(user_interval, timespec::from_time_value(interval))?;
    Ok(0)
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
pub fn sys_sched_rr_get_interval_time64(
    pid: i32,
    user_interval: *mut __kernel_timespec,
) -> AxResult<isize> {
    let interval = TimeValue::from_nanos(scheduler_interval_ns(pid)?);
    write_kernel_timespec(user_interval, __kernel_timespec::from_time_value(interval))?;
    Ok(0)
}

fn sleep_impl(clock: impl Fn() -> TimeValue, dur: TimeValue) -> (AxResult<()>, TimeValue) {
    debug!("sleep_impl <= {dur:?}");

    let start = clock();

    // TODO: currently ignoring concrete clock type
    let task = current_user_task();
    let result = block_on_user(&task, interruptible_for(&task, sleep(dur))).map_err(AxError::from);

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
                write_timespec(rem, timespec::from_time_value(diff))?;
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
                    write_timespec(rem, timespec::from_time_value(diff))?;
                }
            }
            Err(err)
        }
    }
}

pub fn sys_sched_getaffinity(pid: i32, cpusetsize: usize, user_mask: *mut u8) -> AxResult<isize> {
    let cpu_count = hal::cpu_num();
    let kernel_mask_bytes = cpu_count
        .div_ceil(usize::BITS as usize)
        .saturating_mul(core::mem::size_of::<usize>());
    if cpusetsize
        .checked_mul(8)
        .is_none_or(|bits| bits < cpu_count)
        || !cpusetsize.is_multiple_of(core::mem::size_of::<usize>())
    {
        return Err(AxError::InvalidInput);
    }

    let affinity = scheduler::thread_affinity(scheduler_thread_id(pid)?).map_err(map_task_error)?;
    let mut mask_bytes = vec![0_u8; kernel_mask_bytes.min(cpusetsize)];
    for cpu in 0..cpu_count {
        let cpu_id = u32::try_from(cpu).map_err(|_| AxError::InvalidInput)?;
        if affinity.contains(scheduler::CpuId::new(cpu_id)) {
            mask_bytes[cpu / 8] |= 1 << (cpu % 8);
        }
    }

    vm_write_slice(user_mask, &mask_bytes)?;

    Ok(mask_bytes.len() as _)
}

pub fn check_sched_permission(pid: i32) -> AxResult<()> {
    let caller = current_user_task().as_thread().cred();
    let task = get_task(scheduler_tid(pid)?)?;
    if task.id() == current_user_task().id() {
        return Ok(());
    }
    let target_cred = task.as_thread().cred();
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
    let cpu_count = hal::cpu_num();
    let size = cpusetsize.min(cpu_count.div_ceil(8));
    let user_mask = vm_load(user_mask, size)?;
    let mut affinity = scheduler::CpuSet::empty(cpu_count);
    let mut any_cpu = false;

    for i in 0..(size * 8).min(cpu_count) {
        if user_mask[i / 8] & (1 << (i % 8)) != 0 {
            let cpu_id = u32::try_from(i).map_err(|_| AxError::InvalidInput)?;
            affinity.insert(scheduler::CpuId::new(cpu_id));
            any_cpu = true;
        }
    }

    if !any_cpu {
        return Err(AxError::InvalidInput);
    }
    let target_tid = scheduler_tid(pid)?;
    if target_tid == current_user_task().as_thread().tid() {
        scheduler::set_current_thread_affinity(affinity).map_err(map_task_error)?;
    } else {
        scheduler::set_thread_affinity(scheduler_thread_id(pid)?, affinity)
            .map_err(map_task_error)?;
    }

    Ok(0)
}

pub fn sys_sched_getscheduler(pid: i32) -> AxResult<isize> {
    let policy = scheduler_policy(pid)?;
    let mut linux_policy = linux_policy_number(policy);
    if scheduler_reset_on_fork(pid)? {
        linux_policy |= SCHED_RESET_ON_FORK;
    }
    Ok(linux_policy as isize)
}

pub fn sys_sched_setscheduler(pid: i32, policy: i32, param: *const ()) -> AxResult<isize> {
    if param.is_null() {
        return Err(AxError::InvalidInput);
    }
    let user_param = vm_load::<SchedParam>(param.cast(), 1)?
        .into_iter()
        .next()
        .ok_or(AxError::BadState)?;
    let current_policy = scheduler_policy(pid)?;
    let update = parse_setscheduler(
        policy,
        user_param.sched_priority,
        current_policy,
        scheduler_stored_nice(pid, current_policy)?,
    )?;
    apply_scheduler_update(pid, current_policy, update)?;
    Ok(0)
}

pub(crate) fn sys_sched_setattr(
    pid: i32,
    user_attr: *mut SchedAttr,
    flags: u32,
) -> AxResult<isize> {
    if flags != 0 || user_attr.is_null() || pid < 0 {
        return Err(AxError::InvalidInput);
    }
    let attr = load_sched_attr(user_attr)?;
    let current_policy = scheduler_policy(pid)?;
    let update = parse_sched_attr(attr, current_policy)?;
    apply_scheduler_update(pid, current_policy, update)?;
    Ok(0)
}

pub(crate) fn sys_sched_getattr(
    pid: i32,
    user_attr: *mut SchedAttr,
    user_size: usize,
    flags: u32,
) -> AxResult<isize> {
    const SCHED_ATTR_V0_SIZE: usize = 48;
    const MAX_SCHED_ATTR_SIZE: usize = 4096;

    if user_attr.is_null()
        || pid < 0
        || flags != 0
        || !(SCHED_ATTR_V0_SIZE..=MAX_SCHED_ATTR_SIZE).contains(&user_size)
    {
        return Err(AxError::InvalidInput);
    }

    let policy = scheduler_policy(pid)?;
    let mut attr = sched_attr_from_policy(policy, scheduler_reset_on_fork(pid)?);
    attr.size = user_size.min(core::mem::size_of::<SchedAttr>()) as u32;

    let mut output = Vec::new();
    output
        .try_reserve_exact(user_size)
        .map_err(|_| AxError::NoMemory)?;
    output.resize(user_size, 0);
    let attr_bytes = bytemuck::bytes_of(&attr);
    let copy_size = output.len().min(attr_bytes.len());
    output[..copy_size].copy_from_slice(&attr_bytes[..copy_size]);
    vm_write_slice(user_attr.cast::<u8>(), &output)?;
    Ok(0)
}

pub fn sys_sched_getparam(pid: i32, user_param: *mut ()) -> AxResult<isize> {
    if user_param.is_null() {
        return Err(AxError::InvalidInput);
    }
    let output = SchedParam {
        sched_priority: linux_sched_priority(scheduler_policy(pid)?),
    };
    user_param.cast::<SchedParam>().vm_write(output)?;
    Ok(0)
}

pub fn sys_sched_setparam(pid: i32, param: *const ()) -> AxResult<isize> {
    if param.is_null() {
        return Err(AxError::InvalidInput);
    }
    let current_policy = scheduler_policy(pid)?;
    let user_param = vm_load::<SchedParam>(param.cast(), 1)?
        .into_iter()
        .next()
        .ok_or(AxError::BadState)?;
    let mut policy = linux_policy_number(current_policy);
    if scheduler_reset_on_fork(pid)? {
        policy |= SCHED_RESET_ON_FORK;
    }
    let update = parse_setscheduler(
        policy as i32,
        user_param.sched_priority,
        current_policy,
        scheduler_stored_nice(pid, current_policy)?,
    )?;
    apply_scheduler_update(pid, current_policy, update)?;
    Ok(0)
}

fn apply_scheduler_update(
    pid: i32,
    current_policy: scheduler::SchedulePolicy,
    update: ScheduleUpdate,
) -> AxResult<()> {
    check_sched_permission(pid)?;
    let task = get_task(scheduler_tid(pid)?)?;
    let caller = current_user_task().as_thread().cred();
    let (rlimit_rtprio, rlimit_nice) = {
        let limits = task.as_thread().proc_data.rlim.read();
        (limits[RLIMIT_RTPRIO].current, limits[RLIMIT_NICE].current)
    };
    check_policy_permission(
        SchedulerPermission {
            owns_target: true,
            has_cap_sys_nice: caller.has_cap_sys_nice(),
            rlimit_rtprio,
            rlimit_nice,
            stored_nice: scheduler_stored_nice(pid, current_policy)?,
        },
        current_policy,
        update.permission_policy,
    )?;
    let current_reset_on_fork = task.reset_on_fork();
    check_reset_on_fork_permission(
        caller.has_cap_sys_nice(),
        current_reset_on_fork,
        update.reset_on_fork,
    )?;

    let thread = scheduler_thread_id(pid)?;
    scheduler::set_thread_policy(thread, update.policy).map_err(map_task_error)?;

    task.set_reset_on_fork(update.reset_on_fork);
    if let scheduler::SchedulePolicy::Fair { nice, .. } = update.policy {
        task.as_thread().set_nice(i32::from(nice.get()));
    }
    Ok(())
}

fn scheduler_policy(pid: i32) -> AxResult<scheduler::SchedulePolicy> {
    let thread = scheduler_thread_id(pid)?;
    scheduler::thread_policy(thread).map_err(map_task_error)
}

fn scheduler_reset_on_fork(pid: i32) -> AxResult<bool> {
    let task = get_task(scheduler_tid(pid)?)?;
    Ok(task.reset_on_fork())
}

fn scheduler_stored_nice(
    pid: i32,
    current_policy: scheduler::SchedulePolicy,
) -> AxResult<scheduler::Nice> {
    if let scheduler::SchedulePolicy::Fair { nice, .. } = current_policy {
        return Ok(nice);
    }
    let task = get_task(scheduler_tid(pid)?)?;
    let nice = i8::try_from(task.as_thread().nice()).map_err(|_| AxError::BadState)?;
    scheduler::Nice::new(nice).map_err(map_task_error)
}

fn scheduler_interval_ns(pid: i32) -> AxResult<u64> {
    Ok(match scheduler_policy(pid)? {
        scheduler::SchedulePolicy::RoundRobin { quantum_ns, .. } => quantum_ns,
        _ => 0,
    })
}

fn scheduler_thread_id(pid: i32) -> AxResult<scheduler::ThreadId> {
    let target = get_task(scheduler_tid(pid)?)?;
    if let Some(id) = target.as_thread().scheduler_id() {
        return Ok(id);
    }

    // The first switch-in normally binds the identity through the Starry
    // extension hook. This fallback covers the boot thread without ever
    // deriving an identity from its Linux TID.
    if target.id() == current_user_task().id() {
        let id = scheduler::current_thread_id().map_err(map_task_error)?;
        target.as_thread().bind_scheduler_id(id)?;
        return Ok(id);
    }

    Err(AxError::BadState)
}

fn scheduler_tid(pid: i32) -> AxResult<u32> {
    if pid == 0 {
        Ok(current_user_task().as_thread().tid())
    } else {
        u32::try_from(pid).map_err(|_| AxError::InvalidInput)
    }
}

fn load_sched_attr(user_attr: *mut SchedAttr) -> AxResult<SchedAttr> {
    const SCHED_ATTR_V0_SIZE: usize = 48;
    const MAX_SCHED_ATTR_SIZE: usize = 4096;

    let requested_size = user_attr.cast_const().cast::<u32>().vm_read()? as usize;
    let requested_size = if requested_size == 0 {
        SCHED_ATTR_V0_SIZE
    } else {
        requested_size
    };
    if !(SCHED_ATTR_V0_SIZE..=MAX_SCHED_ATTR_SIZE).contains(&requested_size) {
        write_sched_attr_size(user_attr)?;
        return Err(AxError::ArgumentListTooLong);
    }

    let known_size = core::mem::size_of::<SchedAttr>();
    let copy_size = requested_size.min(known_size);
    let input = vm_load(user_attr.cast_const().cast::<u8>(), copy_size)?;
    let mut attr = SchedAttr::zeroed();
    bytemuck::bytes_of_mut(&mut attr)[..copy_size].copy_from_slice(&input);

    if requested_size > known_size {
        let extra = vm_load(
            user_attr.cast_const().cast::<u8>().wrapping_add(known_size),
            requested_size - known_size,
        )?;
        if extra.iter().any(|byte| *byte != 0) {
            write_sched_attr_size(user_attr)?;
            return Err(AxError::ArgumentListTooLong);
        }
        attr.size = known_size as u32;
    }
    Ok(attr)
}

fn write_sched_attr_size(user_attr: *mut SchedAttr) -> AxResult<()> {
    user_attr
        .cast::<u32>()
        .vm_write(core::mem::size_of::<SchedAttr>() as u32)
        .map_err(AxError::from)
}

fn map_task_error(error: scheduler::TaskError) -> AxError {
    use scheduler::TaskError;

    match error {
        TaskError::InvalidConfiguration
        | TaskError::InvalidCpuCount(_)
        | TaskError::InvalidCpu(_)
        | TaskError::InvalidNice(_)
        | TaskError::InvalidRtPriority(_)
        | TaskError::InvalidRoundRobinQuantum
        | TaskError::InvalidDeadline { .. }
        | TaskError::UnsupportedDeadlineFlags(_) => AxError::InvalidInput,
        TaskError::DeadlineAdmission
        | TaskError::DeadlineAffinity
        | TaskError::ActiveTimerAffinity
        | TaskError::ThreadBusy
        | TaskError::ThreadPinned => AxError::ResourceBusy,
        TaskError::StaleThreadId => AxError::NoSuchProcess,
        TaskError::NotInitialized
        | TaskError::InvalidRuntimeHandle
        | TaskError::CpuOwnerBorrowed => AxError::BadState,
        TaskError::UnsafeContext => AxError::OperationNotPermitted,
        TaskError::TimerCapacity => AxError::NoMemory,
        TaskError::CpuOwnerMismatch { .. }
        | TaskError::ExecutorOwnerMismatch { .. }
        | TaskError::CpuAlreadyOnline(_)
        | TaskError::CpuOffline(_)
        | TaskError::InvalidTransition { .. }
        | TaskError::AlreadyQueued
        | TaskError::NotReady
        | TaskError::NotExited
        | TaskError::NoRunnableThread
        | TaskError::InvalidPiState
        | TaskError::PiCycle
        | TaskError::RuntimeFailure(_) => AxError::BadState,
    }
}

pub fn sys_getpriority(which: u32, who: u32) -> AxResult<isize> {
    debug!("sys_getpriority <= which: {which}, who: {who}");

    match which {
        PRIO_PROCESS => match get_task(if who == 0 { 0 } else { who }) {
            Ok(task) => Ok(raw_priority(task.as_thread().nice())),
            Err(AxError::NoSuchProcess) if who != 0 && is_zombie_pid(who) => Ok(20),
            Err(err) => Err(err),
        },
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current_user_task()
                    .as_thread()
                    .proc_data
                    .proc
                    .group()
                    .pgid()
            } else {
                get_process_group(who)?.pgid()
            };
            min_priority_for_tasks(tasks_for_processes(
                processes()
                    .into_iter()
                    .filter(|proc| proc.proc.group().pgid() == pgid),
            ))
        }
        PRIO_USER => {
            let uid = if who == 0 {
                current_user_task().as_thread().cred().uid
            } else {
                who
            };
            min_priority_for_tasks(
                tasks_for_processes(processes())
                    .into_iter()
                    .filter(|task| task.as_thread().cred().uid == uid),
            )
        }
        _ => Err(AxError::InvalidInput),
    }
}

pub fn sys_setpriority(which: u32, who: u32, prio: i32) -> AxResult<isize> {
    debug!("sys_setpriority <= which: {which}, who: {who}, prio: {prio}");

    let nice = prio.clamp(-20, 19);
    match which {
        PRIO_PROCESS => {
            let task = get_task(if who == 0 { 0 } else { who })?;
            check_setpriority_permission(&task, nice)?;
            set_thread_scheduler_nice(&task, nice)?;
            Ok(0)
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current_user_task()
                    .as_thread()
                    .proc_data
                    .proc
                    .group()
                    .pgid()
            } else {
                get_process_group(who)?.pgid()
            };
            set_priority_for_tasks(
                tasks_for_processes(
                    processes()
                        .into_iter()
                        .filter(|proc| proc.proc.group().pgid() == pgid),
                ),
                nice,
            )
        }
        PRIO_USER => {
            let uid = if who == 0 {
                current_user_task().as_thread().cred().uid
            } else {
                who
            };
            set_priority_for_tasks(
                tasks_for_processes(processes())
                    .into_iter()
                    .filter(|task| task.as_thread().cred().uid == uid),
                nice,
            )
        }
        _ => Err(AxError::InvalidInput),
    }
}

fn raw_priority(nice: i32) -> isize {
    (20 - nice) as isize
}

fn min_priority_for_tasks(tasks: impl IntoIterator<Item = UserTaskRef>) -> AxResult<isize> {
    tasks
        .into_iter()
        .map(|task| task.as_thread().nice())
        .min()
        .map(raw_priority)
        .ok_or(AxError::NoSuchProcess)
}

fn tasks_for_processes(processes: impl IntoIterator<Item = Arc<ProcessData>>) -> Vec<UserTaskRef> {
    processes
        .into_iter()
        .flat_map(|proc| proc.proc.threads())
        .filter_map(|tid| get_task(tid).ok())
        .collect()
}

fn setpriority_cred_matches(caller: &Cred, target: &Cred) -> bool {
    caller.euid == target.uid || caller.euid == target.euid
}

fn check_setpriority_permission(task: &UserTaskRef, nice: i32) -> AxResult<()> {
    let caller = current_user_task().as_thread().cred();
    if caller.has_cap_sys_nice() {
        return Ok(());
    }

    let target = task.as_thread().cred();
    if !setpriority_cred_matches(&caller, &target) {
        return Err(AxError::OperationNotPermitted);
    }
    if nice < task.as_thread().nice() {
        let rlimit_nice = task.as_thread().proc_data.rlim.read()[RLIMIT_NICE]
            .current
            .min(40);
        let lowest_allowed = 20_i64 - rlimit_nice as i64;
        if i64::from(nice) < lowest_allowed {
            return Err(AxError::PermissionDenied);
        }
    }
    Ok(())
}

fn set_priority_for_tasks(
    tasks: impl IntoIterator<Item = UserTaskRef>,
    nice: i32,
) -> AxResult<isize> {
    let tasks: Vec<_> = tasks.into_iter().collect();
    if tasks.is_empty() {
        return Err(AxError::NoSuchProcess);
    }
    for task in &tasks {
        check_setpriority_permission(task, nice)?;
    }
    for task in tasks {
        set_thread_scheduler_nice(&task, nice)?;
    }
    Ok(0)
}

fn set_thread_scheduler_nice(task: &UserTaskRef, nice: i32) -> AxResult<()> {
    let nice = scheduler::Nice::new(nice as i8).map_err(map_task_error)?;
    let policy = task.policy();
    if let scheduler::SchedulePolicy::Fair { mode, .. } = policy {
        scheduler::set_thread_policy(task.id(), scheduler::SchedulePolicy::fair(nice, mode))
            .map_err(map_task_error)?;
    }
    task.as_thread().set_nice(i32::from(nice.get()));
    Ok(())
}
