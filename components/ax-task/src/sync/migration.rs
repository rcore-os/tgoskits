//! Migration exclusion for preemptible task-context critical sections.

use alloc::sync::Arc;
use core::marker::PhantomData;

use crate::{
    runtime::{context::runtime_task_system, lock::PreemptScope, task_runtime},
    sched::CpuId,
    thread::{TaskError, ThreadCore, current::current_thread_core_arc},
};

/// Pins the current task to its CPU without disabling preemption or interrupts.
///
/// Pins nest. Remote affinity requests remain pending until the outermost pin
/// is released. The guard cannot be transferred to another task.
#[must_use]
pub struct MigrationGuard {
    current: Arc<ThreadCore>,
    cpu: CpuId,
    _not_send: PhantomData<*mut ()>,
}

impl MigrationGuard {
    /// Acquires a task migration pin; hard IRQ contexts cannot own one.
    pub fn new() -> Result<Self, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _preempt = PreemptScope::enter();
        let current = current_thread_core_arc()?;
        let cpu = runtime_task_system()?.disable_current_migration(&current)?;
        Ok(Self {
            current,
            cpu,
            _not_send: PhantomData,
        })
    }

    /// Returns the CPU retained by this guard, including across preemption.
    pub const fn cpu(&self) -> CpuId {
        self.cpu
    }
}

impl Drop for MigrationGuard {
    fn drop(&mut self) {
        let _preempt = PreemptScope::enter();
        runtime_task_system()
            .and_then(|system| system.enable_current_migration(&self.current, self.cpu))
            .expect("migration guard must release on its owning task and CPU");
    }
}
