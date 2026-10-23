// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Narrow scheduler operations for LinuxKPI mechanisms that need native tasks.

use alloc::{boxed::Box, string::String};

use crate::{config::ENODEV, kernel_services::ProviderSlot};

/// Owned task entry accepted by a host scheduler.
pub type TaskEntry = Box<dyn FnOnce() + Send + 'static>;

/// Supported Linux scheduling classes requested by KAPI worker wrappers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskSchedulingPolicy {
    Normal,
    Fifo,
}

/// Scheduling operations used by deferred work, timers, and RCU callbacks.
///
/// This table is deliberately separate from process credentials and file
/// descriptors in `kernel_services::TaskOps`, so a device runtime does not
/// have to provide user-process policy to schedule its private workers.
pub struct TaskRuntimeOps {
    /// Stable nonzero identity unique among live tasks, or `None` if no task is
    /// current (for example, during early boot or interrupt context).
    pub current_task_id: fn() -> Option<u64>,
    pub current_need_resched: fn() -> bool,
    pub can_block_current: fn() -> bool,
    pub yield_now: fn(),
    /// Sleep for at least `nanos`; returns an error if this context cannot
    /// block. The caller may fall back to cooperative polling where needed.
    pub sleep_ns: fn(nanos: u64) -> Result<(), i32>,
    /// Spawn a task with an explicit Linux scheduling policy. Unsupported
    /// classes must return `-EOPNOTSUPP`; they must not silently become normal.
    pub spawn_with_policy:
        fn(name: String, policy: TaskSchedulingPolicy, entry: TaskEntry) -> Result<(), i32>,
    pub cpu_count: fn() -> usize,
}

static TASK_RUNTIME: ProviderSlot<TaskRuntimeOps> = ProviderSlot::new();

#[cfg(test)]
static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn test_current_task_id() -> Option<u64> {
    None
}

#[cfg(test)]
fn test_current_need_resched() -> bool {
    false
}

#[cfg(test)]
fn test_can_block_current() -> bool {
    true
}

#[cfg(test)]
fn test_yield_now() {
    std::thread::yield_now();
}

#[cfg(test)]
fn test_sleep_ns(nanos: u64) -> Result<(), i32> {
    std::thread::sleep(std::time::Duration::from_nanos(nanos));
    Ok(())
}

#[cfg(test)]
fn test_spawn_with_policy(
    name: String,
    policy: TaskSchedulingPolicy,
    entry: TaskEntry,
) -> Result<(), i32> {
    if policy == TaskSchedulingPolicy::Fifo {
        return Err(-crate::config::EOPNOTSUPP);
    }
    if !name.starts_with("kthread-test") {
        return Err(-crate::config::EAGAIN);
    }
    std::thread::Builder::new()
        .name(name)
        .spawn(entry)
        .map(|_| ())
        .map_err(|_| -crate::config::EAGAIN)
}

#[cfg(test)]
fn test_cpu_count() -> usize {
    1
}

#[cfg(test)]
static TEST_RUNTIME: TaskRuntimeOps = TaskRuntimeOps {
    current_task_id: test_current_task_id,
    current_need_resched: test_current_need_resched,
    can_block_current: test_can_block_current,
    yield_now: test_yield_now,
    sleep_ns: test_sleep_ns,
    spawn_with_policy: test_spawn_with_policy,
    cpu_count: test_cpu_count,
};

/// Serialize KAPI host tests that share the process-global task runtime.
#[cfg(test)]
pub(crate) fn test_serial_runtime() -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if TASK_RUNTIME.get().is_none() {
        TASK_RUNTIME
            .install(&TEST_RUNTIME)
            .expect("test task runtime installation failed");
    }
    guard
}

/// Install the scheduler adapter before starting deferred KAPI work.
pub fn install_task_runtime_ops(ops: &'static TaskRuntimeOps) -> Result<(), &'static str> {
    TASK_RUNTIME.install(ops)
}

fn ops() -> &'static TaskRuntimeOps {
    TASK_RUNTIME.require("task runtime")
}

pub fn current_task_id() -> Option<u64> {
    TASK_RUNTIME
        .get()
        .and_then(|provider| (provider.current_task_id)())
}

pub fn current_need_resched() -> bool {
    TASK_RUNTIME
        .get()
        .is_some_and(|provider| (provider.current_need_resched)())
}

pub fn can_block_current() -> bool {
    #[cfg(test)]
    {
        true
    }
    #[cfg(not(test))]
    {
        TASK_RUNTIME
            .get()
            .is_some_and(|provider| (provider.can_block_current)())
    }
}

pub fn yield_now() {
    (ops().yield_now)();
}

pub fn sleep_ns(nanos: u64) -> Result<(), i32> {
    (ops().sleep_ns)(nanos)
}

pub fn spawn(name: String, entry: TaskEntry) -> Result<(), i32> {
    try_spawn_with_policy(name, TaskSchedulingPolicy::Normal, entry)
}

pub fn spawn_with_policy(
    name: String,
    policy: TaskSchedulingPolicy,
    entry: TaskEntry,
) -> Result<(), i32> {
    try_spawn_with_policy(name, policy, entry)
}

/// Fallible scheduler dispatch for C-facing worker initialization paths.
/// Returns `-ENODEV` when the hosting scheduler adapter is not installed.
pub fn try_spawn_with_policy(
    name: String,
    policy: TaskSchedulingPolicy,
    entry: TaskEntry,
) -> Result<(), i32> {
    let Some(runtime) = TASK_RUNTIME.get() else {
        return Err(-ENODEV);
    };
    (runtime.spawn_with_policy)(name, policy, entry)
}

pub fn cpu_count() -> usize {
    (ops().cpu_count)().max(1)
}
