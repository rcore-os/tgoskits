//! Public host-task facade for long-lived device-worker tasks.
//!
//! Device backends such as the AxVisor virtio-net RX worker need to spawn a
//! host task and block on an event queue without busy-polling. axvm already
//! abstracts the underlying ArceOS task API internally; this module re-exposes
//! just enough of it as a stable public capability so the OS glue can drive
//! workers without depending on `axtask`/`arceos_api` directly.

use alloc::string::String;

use crate::host::task::{
    AxTaskRef, TaskInner, WaitQueueHandle, cpu_mask_from_raw_bits, spawn_task,
};

/// Spawns a worker task with `name` and `stack_size` (in bytes) running `f`.
///
/// Returns the task reference so the caller can keep the task alive and
/// coordinate shutdown.
pub fn spawn_worker_task<F>(name: String, stack_size: usize, f: F) -> WorkerTask
where
    F: FnOnce() + Send + 'static,
{
    let task = TaskInner::new(f, name, stack_size);
    WorkerTask(spawn_task(task))
}

/// Spawns a worker restricted to the host CPUs selected by `cpu_mask`.
///
/// This is useful with non-preemptive schedulers where a continuously runnable
/// vCPU would otherwise starve a deferred device worker on the same run queue.
/// `cpu_mask` must contain at least one initialized host CPU.
pub fn spawn_worker_task_with_affinity<F>(
    name: String,
    stack_size: usize,
    cpu_mask: usize,
    f: F,
) -> WorkerTask
where
    F: FnOnce() + Send + 'static,
{
    assert!(cpu_mask != 0, "worker CPU affinity must not be empty");
    let task = TaskInner::new(f, name, stack_size);
    task.set_cpumask(cpu_mask_from_raw_bits(cpu_mask));
    WorkerTask(spawn_task(task))
}

/// Returns the number of initialized host CPUs available to workers.
pub fn host_cpu_count() -> usize {
    crate::host::arceos::host_cpu_count()
}

/// Owned handle for a long-lived host worker.
pub struct WorkerTask(AxTaskRef);

impl WorkerTask {
    /// Waits until the worker exits and returns its task exit code.
    #[cfg(not(test))]
    pub fn join(&self) -> i32 {
        self.0.join()
    }

    /// Host unit tests do not link an ArceOS image or its task linker symbols.
    #[cfg(test)]
    pub fn join(&self) -> i32 {
        0
    }
}

/// Yields the current task to the scheduler.
pub fn yield_now() {
    crate::host::arceos::yield_now();
}

/// Event queue used to block and wake a worker task without busy-polling.
///
/// Wraps the host wait-queue primitive. A worker calls [`WorkerWaitQueue::wait_until`]
/// with a readiness condition; a producer calls [`WorkerWaitQueue::wake_one`]
/// after making new work available or to release a shutting-down worker.
pub struct WorkerWaitQueue(WaitQueueHandle);

impl WorkerWaitQueue {
    /// Creates an empty event queue.
    pub const fn new() -> Self {
        Self(WaitQueueHandle::new())
    }
}

impl Default for WorkerWaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerWaitQueue {
    /// Blocks the current task until `condition` is true, getting re-checked on
    /// every wake. Use this instead of sleep-polling in the worker hot loop.
    pub fn wait_until(&self, condition: impl Fn() -> bool) {
        crate::host::arceos::wait_queue_wait_until(&self.0, condition);
    }

    /// Wakes one waiter (if any).
    pub fn wake_one(&self) {
        crate::host::arceos::wait_queue_wake(&self.0, 1);
    }

    /// Wakes every waiter.
    pub fn wake_all(&self) {
        crate::host::arceos::wait_queue_wake(&self.0, u32::MAX);
    }
}
