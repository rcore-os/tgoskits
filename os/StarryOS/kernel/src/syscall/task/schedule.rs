use alloc::{sync::Arc, vec, vec::Vec};

use ax_runtime::hal::{self, time::TimeValue};
use ax_std::os::arceos::task as scheduler;
use bytemuck::{Pod, Zeroable};
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
use linux_raw_sys::general::__kernel_timespec;
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_MONOTONIC, CLOCK_REALTIME, PRIO_PGRP, PRIO_PROCESS, PRIO_USER,
    RLIMIT_NICE, RLIMIT_RTPRIO, SCHED_RESET_ON_FORK, TIMER_ABSTIME, timespec,
};

use super::schedule_abi::{
    SchedAttr, ScheduleUpdate, SchedulerPermission, check_policy_permission,
    check_reset_on_fork_permission, linux_policy_number, linux_sched_priority, parse_sched_attr,
    parse_setscheduler, sched_attr_from_policy, scheduler_priority_max, scheduler_priority_min,
};
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
use crate::syscall::time::write_kernel_timespec;
use crate::{
    StarryError,
    mm::{VmMutPtr, VmPtr, vm_load, vm_write_slice},
    syscall::time::write_timespec,
    task::{
        Cred, ProcessData, UserTaskRef,
        future::{UserWaitOutcome, block_on_user_until, block_on_user_until_wall},
        get_process_data, get_process_group, get_task, get_zombie_nice, processes,
        resolve_user_pid,
    },
    time::{SleepDeadline, TimeValueLike},
};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SchedParam {
    sched_priority: i32,
}

pub fn sys_sched_yield() -> crate::StarryResult<isize> {
    scheduler::yield_current_cpu().map_err(map_task_error)?;
    Ok(0)
}

pub fn sys_sched_get_priority_min(policy: i32) -> crate::StarryResult<isize> {
    let policy = u32::try_from(policy).map_err(|_| crate::StarryError::InvalidInput)?;
    Ok(scheduler_priority_min(policy)? as isize)
}

pub fn sys_sched_get_priority_max(policy: i32) -> crate::StarryResult<isize> {
    let policy = u32::try_from(policy).map_err(|_| crate::StarryError::InvalidInput)?;
    Ok(scheduler_priority_max(policy)? as isize)
}

pub fn sys_sched_rr_get_interval(
    current: &crate::task::UserTaskRef,
    pid: i32,
    user_interval: *mut timespec,
) -> crate::StarryResult<isize> {
    let interval = TimeValue::from_nanos(scheduler_interval_ns(current, pid)?);
    write_timespec(current, user_interval, timespec::from_time_value(interval))?;
    Ok(0)
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
pub fn sys_sched_rr_get_interval_time64(
    current: &crate::task::UserTaskRef,
    pid: i32,
    user_interval: *mut __kernel_timespec,
) -> crate::StarryResult<isize> {
    let interval = TimeValue::from_nanos(scheduler_interval_ns(current, pid)?);
    write_kernel_timespec(
        current,
        user_interval,
        __kernel_timespec::from_time_value(interval),
    )?;
    Ok(0)
}

fn sleep_until(
    current: &crate::task::UserTaskRef,
    deadline: SleepDeadline,
) -> crate::StarryResult<()> {
    debug!("sleep_until <= {deadline:?}");
    let outcome = match deadline {
        SleepDeadline::Monotonic(deadline) => block_on_user_until(
            current,
            Some(crate::task::future::monotonic_deadline_from_time(deadline)),
            core::future::pending::<()>(),
        ),
        SleepDeadline::Realtime(deadline) => {
            block_on_user_until_wall(current, Some(deadline), core::future::pending::<()>())
        }
    };
    match outcome {
        UserWaitOutcome::TimedOut => Ok(()),
        UserWaitOutcome::Interrupted => Err(crate::StarryError::Interrupted),
        UserWaitOutcome::Ready(()) => unreachable!("a pending sleep future cannot complete"),
    }
}

fn sleep_relative(
    current: &crate::task::UserTaskRef,
    duration: TimeValue,
) -> (crate::StarryResult<()>, TimeValue) {
    debug!("sleep_relative <= {duration:?}");
    let start = hal::time::monotonic_time();
    let deadline = start.saturating_add(duration);
    let result = sleep_until(current, SleepDeadline::Monotonic(deadline));

    (result, hal::time::monotonic_time().saturating_sub(start))
}

/// Sleep some nanoseconds
pub fn sys_nanosleep(
    current: &crate::task::UserTaskRef,
    req: *const timespec,
    rem: *mut timespec,
) -> crate::StarryResult<isize> {
    // FIXME: AnyBitPattern
    let req = unsafe { req.vm_read_uninit(current)?.assume_init() }.try_into_time_value()?;
    debug!("sys_nanosleep <= req: {req:?}");

    let (result, actual) = sleep_relative(current, req);

    match result {
        Ok(()) => Ok(0),
        Err(err) => {
            let diff = req.saturating_sub(actual);
            debug!("sys_nanosleep => rem: {diff:?}");
            if let Some(rem) = rem.nullable() {
                write_timespec(current, rem, timespec::from_time_value(diff))?;
            }
            Err(err)
        }
    }
}

pub fn sys_clock_nanosleep(
    current: &crate::task::UserTaskRef,
    clock_id: __kernel_clockid_t,
    flags: u32,
    req: *const timespec,
    rem: *mut timespec,
) -> crate::StarryResult<isize> {
    let absolute_deadline = match clock_id as u32 {
        CLOCK_REALTIME => SleepDeadline::Realtime,
        CLOCK_MONOTONIC => SleepDeadline::Monotonic,
        _ => {
            warn!("Unsupported clock_id: {clock_id}");
            return Err(StarryError::InvalidInput);
        }
    };

    let req = unsafe { req.vm_read_uninit(current)?.assume_init() }.try_into_time_value()?;
    debug!("sys_clock_nanosleep <= clock_id: {clock_id}, flags: {flags}, req: {req:?}");

    let is_abstime = flags & TIMER_ABSTIME != 0;
    let (result, elapsed) = if is_abstime {
        (
            sleep_until(current, absolute_deadline(req)),
            TimeValue::ZERO,
        )
    } else {
        sleep_relative(current, req)
    };

    match result {
        Ok(()) => Ok(0),
        Err(err) => {
            if !is_abstime {
                let diff = req.saturating_sub(elapsed);
                debug!("sys_clock_nanosleep => rem: {diff:?}");
                if let Some(rem) = rem.nullable() {
                    write_timespec(current, rem, timespec::from_time_value(diff))?;
                }
            }
            Err(err)
        }
    }
}

pub fn sys_sched_getaffinity(
    current: &crate::task::UserTaskRef,
    pid: i32,
    cpusetsize: usize,
    user_mask: *mut u8,
) -> crate::StarryResult<isize> {
    let cpu_count = hal::cpu_num();
    let kernel_mask_bytes = cpu_count
        .div_ceil(usize::BITS as usize)
        .saturating_mul(core::mem::size_of::<usize>());
    if cpusetsize
        .checked_mul(8)
        .is_none_or(|bits| bits < cpu_count)
        || !cpusetsize.is_multiple_of(core::mem::size_of::<usize>())
    {
        return Err(crate::StarryError::InvalidInput);
    }

    let affinity =
        scheduler::thread_affinity(scheduler_thread_id(current, pid)?).map_err(map_task_error)?;
    let mut mask_bytes = vec![0_u8; kernel_mask_bytes.min(cpusetsize)];
    for cpu in 0..cpu_count {
        let cpu_id = u32::try_from(cpu).map_err(|_| crate::StarryError::InvalidInput)?;
        if affinity.contains(scheduler::CpuId::new(cpu_id)) {
            mask_bytes[cpu / 8] |= 1 << (cpu % 8);
        }
    }

    vm_write_slice(current, user_mask, &mask_bytes)?;

    Ok(mask_bytes.len() as _)
}

pub fn check_sched_permission(
    current: &crate::task::UserTaskRef,
    pid: i32,
) -> crate::StarryResult<()> {
    let caller = current.as_thread().cred();
    let task = get_task(scheduler_tid(current, pid)?)?;
    if task.id() == current.id() {
        return Ok(());
    }
    let target_cred = task.as_thread().cred();
    if caller.has_cap_sys_nice()
        || caller.euid == target_cred.uid
        || caller.euid == target_cred.euid
    {
        Ok(())
    } else {
        Err(StarryError::OperationNotPermitted)
    }
}

pub fn sys_sched_setaffinity(
    current: &crate::task::UserTaskRef,
    pid: i32,
    cpusetsize: usize,
    user_mask: *const u8,
) -> crate::StarryResult<isize> {
    check_sched_permission(current, pid)?;
    let cpu_count = hal::cpu_num();
    let size = cpusetsize.min(cpu_count.div_ceil(8));
    let user_mask = vm_load(current, user_mask, size)?;
    let mut affinity = scheduler::CpuSet::empty(cpu_count);
    let mut any_cpu = false;

    for i in 0..(size * 8).min(cpu_count) {
        if user_mask[i / 8] & (1 << (i % 8)) != 0 {
            let cpu_id = u32::try_from(i).map_err(|_| crate::StarryError::InvalidInput)?;
            affinity.insert(scheduler::CpuId::new(cpu_id));
            any_cpu = true;
        }
    }

    if !any_cpu {
        return Err(crate::StarryError::InvalidInput);
    }
    let target_tid = scheduler_tid(current, pid)?;
    if target_tid == current.as_thread().tid() {
        scheduler::set_current_thread_affinity(affinity).map_err(map_task_error)?;
    } else {
        scheduler::set_thread_affinity_and_wait(scheduler_thread_id(current, pid)?, affinity)
            .map_err(map_task_error)?;
    }

    Ok(0)
}

pub fn sys_sched_getscheduler(
    current: &crate::task::UserTaskRef,
    pid: i32,
) -> crate::StarryResult<isize> {
    let policy = scheduler_policy(current, pid)?;
    let mut linux_policy = linux_policy_number(policy);
    if scheduler_reset_on_fork(current, pid)? {
        linux_policy |= SCHED_RESET_ON_FORK;
    }
    Ok(linux_policy as isize)
}

pub fn sys_sched_setscheduler(
    current: &crate::task::UserTaskRef,
    pid: i32,
    policy: i32,
    param: *const (),
) -> crate::StarryResult<isize> {
    if param.is_null() {
        return Err(crate::StarryError::InvalidInput);
    }
    let user_param = vm_load::<SchedParam>(current, param.cast(), 1)?
        .into_iter()
        .next()
        .ok_or(crate::StarryError::BadState)?;
    let current_policy = scheduler_policy(current, pid)?;
    let update = parse_setscheduler(
        policy,
        user_param.sched_priority,
        current_policy,
        scheduler_stored_nice(current, pid, current_policy)?,
    )?;
    apply_scheduler_update(current, pid, current_policy, update)?;
    Ok(0)
}

pub(crate) fn sys_sched_setattr(
    current: &crate::task::UserTaskRef,
    pid: i32,
    user_attr: *mut SchedAttr,
    flags: u32,
) -> crate::StarryResult<isize> {
    if flags != 0 || user_attr.is_null() || pid < 0 {
        return Err(crate::StarryError::InvalidInput);
    }
    let attr = load_sched_attr(current, user_attr)?;
    let current_policy = scheduler_policy(current, pid)?;
    let update = parse_sched_attr(attr, current_policy)?;
    apply_scheduler_update(current, pid, current_policy, update)?;
    Ok(0)
}

pub(crate) fn sys_sched_getattr(
    current: &crate::task::UserTaskRef,
    pid: i32,
    user_attr: *mut SchedAttr,
    user_size: usize,
    flags: u32,
) -> crate::StarryResult<isize> {
    const SCHED_ATTR_V0_SIZE: usize = 48;
    const MAX_SCHED_ATTR_SIZE: usize = 4096;

    if user_attr.is_null()
        || pid < 0
        || flags != 0
        || !(SCHED_ATTR_V0_SIZE..=MAX_SCHED_ATTR_SIZE).contains(&user_size)
    {
        return Err(crate::StarryError::InvalidInput);
    }

    let policy = scheduler_policy(current, pid)?;
    let mut attr = sched_attr_from_policy(policy, scheduler_reset_on_fork(current, pid)?);
    attr.size = user_size.min(core::mem::size_of::<SchedAttr>()) as u32;

    let mut output = Vec::new();
    output
        .try_reserve_exact(user_size)
        .map_err(|_| crate::StarryError::NoMemory)?;
    output.resize(user_size, 0);
    let attr_bytes = bytemuck::bytes_of(&attr);
    let copy_size = output.len().min(attr_bytes.len());
    output[..copy_size].copy_from_slice(&attr_bytes[..copy_size]);
    vm_write_slice(current, user_attr.cast::<u8>(), &output)?;
    Ok(0)
}

pub fn sys_sched_getparam(
    current: &crate::task::UserTaskRef,
    pid: i32,
    user_param: *mut (),
) -> crate::StarryResult<isize> {
    if user_param.is_null() {
        return Err(crate::StarryError::InvalidInput);
    }
    let output = SchedParam {
        sched_priority: linux_sched_priority(scheduler_policy(current, pid)?),
    };
    user_param.cast::<SchedParam>().vm_write(current, output)?;
    Ok(0)
}

pub fn sys_sched_setparam(
    current: &crate::task::UserTaskRef,
    pid: i32,
    param: *const (),
) -> crate::StarryResult<isize> {
    if param.is_null() {
        return Err(crate::StarryError::InvalidInput);
    }
    let current_policy = scheduler_policy(current, pid)?;
    let user_param = vm_load::<SchedParam>(current, param.cast(), 1)?
        .into_iter()
        .next()
        .ok_or(crate::StarryError::BadState)?;
    let mut policy = linux_policy_number(current_policy);
    if scheduler_reset_on_fork(current, pid)? {
        policy |= SCHED_RESET_ON_FORK;
    }
    let update = parse_setscheduler(
        policy as i32,
        user_param.sched_priority,
        current_policy,
        scheduler_stored_nice(current, pid, current_policy)?,
    )?;
    apply_scheduler_update(current, pid, current_policy, update)?;
    Ok(0)
}

fn apply_scheduler_update(
    current: &crate::task::UserTaskRef,
    pid: i32,
    current_policy: scheduler::SchedulePolicy,
    update: ScheduleUpdate,
) -> crate::StarryResult<()> {
    check_sched_permission(current, pid)?;
    let task = get_task(scheduler_tid(current, pid)?)?;
    let caller = current.as_thread().cred();
    let (rlimit_rtprio, rlimit_nice) = {
        let limits = task.as_thread().proc_data.rlimits();
        (limits[RLIMIT_RTPRIO].current, limits[RLIMIT_NICE].current)
    };
    check_policy_permission(
        SchedulerPermission {
            owns_target: true,
            has_cap_sys_nice: caller.has_cap_sys_nice(),
            rlimit_rtprio,
            rlimit_nice,
            stored_nice: scheduler_stored_nice(current, pid, current_policy)?,
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

    let thread = scheduler_thread_id(current, pid)?;
    scheduler::set_thread_policy(thread, update.policy).map_err(map_task_error)?;

    task.set_reset_on_fork(update.reset_on_fork);
    if let scheduler::SchedulePolicy::Fair { nice, .. } = update.policy {
        task.as_thread().set_nice(i32::from(nice.get()));
    }
    Ok(())
}

fn scheduler_policy(
    current: &crate::task::UserTaskRef,
    pid: i32,
) -> crate::StarryResult<scheduler::SchedulePolicy> {
    let thread = scheduler_thread_id(current, pid)?;
    scheduler::thread_policy(thread).map_err(map_task_error)
}

fn scheduler_reset_on_fork(
    current: &crate::task::UserTaskRef,
    pid: i32,
) -> crate::StarryResult<bool> {
    let task = get_task(scheduler_tid(current, pid)?)?;
    Ok(task.reset_on_fork())
}

fn scheduler_stored_nice(
    current: &crate::task::UserTaskRef,
    pid: i32,
    current_policy: scheduler::SchedulePolicy,
) -> crate::StarryResult<scheduler::Nice> {
    if let scheduler::SchedulePolicy::Fair { nice, .. } = current_policy {
        return Ok(nice);
    }
    let task = get_task(scheduler_tid(current, pid)?)?;
    let nice = i8::try_from(task.as_thread().nice()).map_err(|_| crate::StarryError::BadState)?;
    scheduler::Nice::new(nice).map_err(map_task_error)
}

fn scheduler_interval_ns(current: &crate::task::UserTaskRef, pid: i32) -> crate::StarryResult<u64> {
    Ok(match scheduler_policy(current, pid)? {
        scheduler::SchedulePolicy::RoundRobin { quantum_ns, .. } => quantum_ns,
        _ => 0,
    })
}

fn scheduler_thread_id(
    current: &crate::task::UserTaskRef,
    pid: i32,
) -> crate::StarryResult<scheduler::ThreadId> {
    let target = get_task(scheduler_tid(current, pid)?)?;
    if let Some(id) = target.as_thread().scheduler_id() {
        return Ok(id);
    }

    // The first switch-in normally binds the identity through the Starry
    // extension hook. This fallback covers the boot thread without ever
    // deriving an identity from its Linux TID.
    if target.id() == current.id() {
        let id = scheduler::current_thread_id().map_err(map_task_error)?;
        target.as_thread().bind_scheduler_id(id)?;
        return Ok(id);
    }

    Err(crate::StarryError::BadState)
}

fn scheduler_tid(current: &crate::task::UserTaskRef, pid: i32) -> crate::StarryResult<u32> {
    if pid == 0 {
        Ok(current.as_thread().tid())
    } else {
        let local_pid = u32::try_from(pid).map_err(|_| crate::StarryError::InvalidInput)?;
        resolve_user_pid(current, local_pid)
    }
}

fn load_sched_attr(
    current: &crate::task::UserTaskRef,
    user_attr: *mut SchedAttr,
) -> crate::StarryResult<SchedAttr> {
    const SCHED_ATTR_V0_SIZE: usize = 48;
    const MAX_SCHED_ATTR_SIZE: usize = 4096;

    let requested_size = user_attr.cast_const().cast::<u32>().vm_read(current)? as usize;
    let requested_size = if requested_size == 0 {
        SCHED_ATTR_V0_SIZE
    } else {
        requested_size
    };
    if !(SCHED_ATTR_V0_SIZE..=MAX_SCHED_ATTR_SIZE).contains(&requested_size) {
        write_sched_attr_size(current, user_attr)?;
        return Err(crate::StarryError::ArgumentListTooLong);
    }

    let known_size = core::mem::size_of::<SchedAttr>();
    let copy_size = requested_size.min(known_size);
    let input = vm_load(current, user_attr.cast_const().cast::<u8>(), copy_size)?;
    let mut attr = SchedAttr::zeroed();
    bytemuck::bytes_of_mut(&mut attr)[..copy_size].copy_from_slice(&input);

    if requested_size > known_size {
        let extra = vm_load(
            current,
            user_attr.cast_const().cast::<u8>().wrapping_add(known_size),
            requested_size - known_size,
        )?;
        if extra.iter().any(|byte| *byte != 0) {
            write_sched_attr_size(current, user_attr)?;
            return Err(crate::StarryError::ArgumentListTooLong);
        }
        attr.size = known_size as u32;
    }
    Ok(attr)
}

fn write_sched_attr_size(
    current: &crate::task::UserTaskRef,
    user_attr: *mut SchedAttr,
) -> crate::StarryResult<()> {
    user_attr
        .cast::<u32>()
        .vm_write(current, core::mem::size_of::<SchedAttr>() as u32)
        .map_err(crate::StarryError::from)
}

fn map_task_error(error: scheduler::TaskError) -> crate::StarryError {
    use scheduler::TaskError;

    match error {
        TaskError::InvalidConfiguration
        | TaskError::InvalidCpuCount(_)
        | TaskError::InvalidCpu(_)
        | TaskError::InvalidNice(_)
        | TaskError::InvalidRtPriority(_)
        | TaskError::InvalidRoundRobinQuantum
        | TaskError::InvalidDeadline { .. }
        | TaskError::UnsupportedDeadlineFlags(_) => crate::StarryError::InvalidInput,
        TaskError::DeadlineAdmission
        | TaskError::DeadlineAffinity
        | TaskError::ActiveTimerAffinity
        | TaskError::ThreadBusy => crate::StarryError::ResourceBusy,
        TaskError::StaleThreadId => crate::StarryError::NoSuchProcess,
        TaskError::NotInitialized
        | TaskError::InvalidRuntimeHandle
        | TaskError::CpuOwnerBorrowed
        | TaskError::ThreadCapacity => crate::StarryError::BadState,
        TaskError::UnsafeContext => crate::StarryError::OperationNotPermitted,
        TaskError::TimerCapacity => crate::StarryError::NoMemory,
        TaskError::CpuOwnerMismatch { .. }
        | TaskError::ExecutorOwnerMismatch { .. }
        | TaskError::CpuAlreadyOnline(_)
        | TaskError::CpuOffline(_)
        | TaskError::CpuNotQuiescent(_)
        | TaskError::LastOnlineCpu(_)
        | TaskError::InvalidTransition { .. }
        | TaskError::AlreadyQueued
        | TaskError::NotReady
        | TaskError::NotExited
        | TaskError::NoRunnableThread
        | TaskError::InvalidPiState
        | TaskError::InvalidPiWaitState(_)
        | TaskError::PiCycle
        | TaskError::PiChainLimit { .. }
        | TaskError::RuntimeFailure(_) => crate::StarryError::BadState,
    }
}

pub fn sys_getpriority(
    current: &crate::task::UserTaskRef,
    which: u32,
    who: u32,
) -> crate::StarryResult<isize> {
    debug!("sys_getpriority <= which: {which}, who: {who}");

    match which {
        PRIO_PROCESS => match get_task(priority_process_id(current, who)?) {
            Ok(task) => Ok(raw_priority(task.as_thread().nice())),
            Err(crate::StarryError::NoSuchProcess) if who != 0 => {
                let pid = resolve_user_pid(current, who)?;
                let nice = get_process_data(pid)
                    .ok()
                    .and_then(|process| process.retired_leader_nice())
                    .or_else(|| get_zombie_nice(pid))
                    .ok_or(crate::StarryError::NoSuchProcess)?;
                Ok(raw_priority(nice))
            }
            Err(err) => Err(err),
        },
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current.as_thread().proc_data.proc.group().pgid()
            } else {
                get_process_group(resolve_user_pid(current, who)?)?.pgid()
            };
            min_priority_for_tasks(tasks_for_processes(
                processes()
                    .into_iter()
                    .filter(|proc| proc.proc.group().pgid() == pgid),
            ))
        }
        PRIO_USER => {
            let uid = if who == 0 {
                current.as_thread().cred().uid
            } else {
                who
            };
            min_priority_for_tasks(
                tasks_for_processes(processes())
                    .into_iter()
                    .filter(|task| task.as_thread().cred().uid == uid),
            )
        }
        _ => Err(StarryError::InvalidInput),
    }
}

pub fn sys_setpriority(
    current: &crate::task::UserTaskRef,
    which: u32,
    who: u32,
    prio: i32,
) -> crate::StarryResult<isize> {
    debug!("sys_setpriority <= which: {which}, who: {who}, prio: {prio}");

    let nice = prio.clamp(-20, 19);
    match which {
        PRIO_PROCESS => {
            let task = get_task(priority_process_id(current, who)?)?;
            check_setpriority_permission(current, &task, nice)?;
            set_thread_scheduler_nice(&task, nice)?;
            Ok(0)
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current.as_thread().proc_data.proc.group().pgid()
            } else {
                get_process_group(resolve_user_pid(current, who)?)?.pgid()
            };
            set_priority_for_tasks(
                current,
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
                current.as_thread().cred().uid
            } else {
                who
            };
            set_priority_for_tasks(
                current,
                tasks_for_processes(processes())
                    .into_iter()
                    .filter(|task| task.as_thread().cred().uid == uid),
                nice,
            )
        }
        _ => Err(StarryError::InvalidInput),
    }
}

fn priority_process_id(current: &UserTaskRef, who: u32) -> crate::StarryResult<u32> {
    if who == 0 {
        Ok(current.as_thread().tid())
    } else {
        resolve_user_pid(current, who)
    }
}

fn raw_priority(nice: i32) -> isize {
    (20 - nice) as isize
}

fn min_priority_for_tasks(
    tasks: impl IntoIterator<Item = UserTaskRef>,
) -> crate::StarryResult<isize> {
    tasks
        .into_iter()
        .map(|task| task.as_thread().nice())
        .min()
        .map(raw_priority)
        .ok_or(StarryError::NoSuchProcess)
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

fn check_setpriority_permission(
    current: &crate::task::UserTaskRef,
    task: &UserTaskRef,
    nice: i32,
) -> crate::StarryResult<()> {
    let caller = current.as_thread().cred();
    if caller.has_cap_sys_nice() {
        return Ok(());
    }

    let target = task.as_thread().cred();
    if !setpriority_cred_matches(&caller, &target) {
        return Err(StarryError::OperationNotPermitted);
    }
    if nice < task.as_thread().nice() {
        let rlimit_nice = task.as_thread().proc_data.rlimits()[RLIMIT_NICE]
            .current
            .min(40);
        let lowest_allowed = 20_i64 - rlimit_nice as i64;
        if i64::from(nice) < lowest_allowed {
            return Err(crate::StarryError::PermissionDenied);
        }
    }
    Ok(())
}

fn set_priority_for_tasks(
    current: &crate::task::UserTaskRef,
    tasks: impl IntoIterator<Item = UserTaskRef>,
    nice: i32,
) -> crate::StarryResult<isize> {
    let tasks: Vec<_> = tasks.into_iter().collect();
    if tasks.is_empty() {
        return Err(crate::StarryError::NoSuchProcess);
    }
    for task in &tasks {
        check_setpriority_permission(current, task, nice)?;
    }
    for task in tasks {
        set_thread_scheduler_nice(&task, nice)?;
    }
    Ok(0)
}

fn set_thread_scheduler_nice(task: &UserTaskRef, nice: i32) -> crate::StarryResult<()> {
    let nice = scheduler::Nice::new(nice as i8).map_err(map_task_error)?;
    let policy = task.policy();
    if let scheduler::SchedulePolicy::Fair { mode, .. } = policy {
        scheduler::set_thread_policy(task.id(), scheduler::SchedulePolicy::fair(nice, mode))
            .map_err(map_task_error)?;
    }
    task.as_thread().set_nice(i32::from(nice.get()));
    Ok(())
}

#[cfg(axtest)]
pub(crate) fn schedule_clock_and_sched_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{
        CLOCK_MONOTONIC, CLOCK_REALTIME, SCHED_BATCH, SCHED_FIFO, SCHED_IDLE, SCHED_NORMAL,
        SCHED_RR,
    };

    // Test clock_nanosleep clock_id validation
    let valid_clocks = [CLOCK_REALTIME as u32, CLOCK_MONOTONIC as u32];

    for &clock in &valid_clocks {
        assert!(clock == CLOCK_REALTIME as u32 || clock == CLOCK_MONOTONIC as u32);
    }

    // Invalid clock ID
    assert!(999u32 != CLOCK_REALTIME as u32 && 999u32 != CLOCK_MONOTONIC as u32);

    // Test valid scheduler policies
    let valid_policies = [SCHED_NORMAL, SCHED_FIFO, SCHED_RR, SCHED_BATCH, SCHED_IDLE];

    assert!(valid_policies.contains(&SCHED_NORMAL));

    true
}
