//! Bind deferred DRM work to the native scheduler, including kernel threads.

use alloc::string::String;
use core::time::Duration;

use ax_task::{
    runtime::{RuntimeStatus, task_runtime},
    sched::{RtPriority, SchedulePolicy},
    thread::{TaskError, ThreadBuilder, current},
};
use drm_kapi::{
    config::{EAGAIN, EINVAL, EIO, ENODEV, ENOMEM},
    task_runtime::{TaskEntry, TaskRuntimeOps, TaskSchedulingPolicy},
};

pub(super) static OPS: TaskRuntimeOps = TaskRuntimeOps {
    current_task_id,
    current_need_resched: task_runtime::current_preemption_pending,
    can_block_current: || current::validate_blocking_context().is_ok(),
    yield_now: super::relax,
    sleep_ns,
    spawn_with_policy,
    cpu_count: ax_hal::cpu_num,
};

fn current_task_id() -> Option<u64> {
    if task_runtime::in_hard_irq() {
        None
    } else {
        current::current_thread_id().ok().map(|id| id.as_u64())
    }
}

fn task_error(error: TaskError) -> i32 {
    match error {
        TaskError::NotInitialized | TaskError::NoRunnableThread => -ENODEV,
        TaskError::UnsafeContext | TaskError::ThreadCapacity | TaskError::TimerCapacity => -EAGAIN,
        TaskError::InvalidConfiguration => -EINVAL,
        TaskError::RuntimeFailure(status) if status == RuntimeStatus::NoMemory as u32 => -ENOMEM,
        _ => {
            log::error!("DRM task runtime: {error}");
            -EIO
        }
    }
}

fn sleep_ns(nanos: u64) -> Result<(), i32> {
    current::validate_blocking_context().map_err(task_error)?;
    current::sleep(Duration::from_nanos(nanos));
    Ok(())
}

pub(super) fn spawn(name: String, entry: TaskEntry) -> Result<(), i32> {
    spawn_with_policy(name, TaskSchedulingPolicy::Normal, entry)
}

fn spawn_with_policy(
    name: String,
    policy: TaskSchedulingPolicy,
    entry: TaskEntry,
) -> Result<(), i32> {
    current::validate_blocking_context().map_err(task_error)?;
    let builder = ThreadBuilder::new(name);
    let builder = match policy {
        TaskSchedulingPolicy::Normal => builder,
        TaskSchedulingPolicy::Fifo => builder.policy(SchedulePolicy::Fifo {
            // Linux sched_set_fifo() uses MAX_RT_PRIO / 2.
            priority: RtPriority::new(50).expect("Linux worker FIFO priority is in range"),
        }),
    };
    let handle = builder.spawn(entry).map_err(task_error)?;
    // Published threads are retained by the scheduler until exit. KAPI owns
    // each service's stop/flush state; dropping this external inspection lease
    // detaches it, rather than leaking a handle that would prevent reaping.
    drop(handle);
    Ok(())
}
