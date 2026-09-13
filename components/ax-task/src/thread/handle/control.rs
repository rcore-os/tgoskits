//! Thread control through one generation-valid management lease.

use super::*;
use crate::{
    runtime::context::{runtime_task_system, validate_task_context},
    sched::policy::SchedulePolicy,
    thread::{
        current::{current_thread_id, set_current_thread_affinity, validate_blocking_context},
        error::TaskError,
        spec::CpuSet,
    },
};

impl ThreadHandle {
    /// Updates a thread scheduling policy through its owner CPU.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] in hard IRQ context and propagates
    /// policy validation, Deadline admission, identity, and CPU publication
    /// failures.
    pub fn set_policy(&self, policy: SchedulePolicy) -> Result<(), TaskError> {
        let thread = self.id();
        validate_task_context()?;
        runtime_task_system()?.set_thread_policy(thread, policy)
    }

    /// Returns a copy of a thread's CPU affinity.
    pub fn affinity(&self) -> Result<CpuSet, TaskError> {
        let thread = self.id();
        runtime_task_system()?.thread_affinity(thread)
    }

    /// Requests an affinity change and returns its owner-runqueue completion.
    ///
    /// Dropping the completion leaves the request asynchronous. Use `wait()` or
    /// [`Self::set_affinity_and_wait`] when placement must finish before return.
    pub fn request_affinity(
        &self,
        affinity: CpuSet,
    ) -> Result<crate::sched::ThreadAffinityChange, TaskError> {
        let thread = self.id();
        validate_task_context()?;
        runtime_task_system()?.request_thread_affinity(thread, affinity)
    }

    /// Updates a remote thread's affinity and waits for owner-runqueue completion.
    ///
    /// A successful return guarantees that this update was ordered through the
    /// target's owner runqueue. If no later setter superseded it, the target no
    /// longer executes on, is queued on, or has an in-flight transfer to a CPU
    /// excluded by this affinity. Setters that join the same outstanding owner
    /// transition share the target's monotonically increasing completion sequence.
    pub fn set_affinity_and_wait(&self, affinity: CpuSet) -> Result<(), TaskError> {
        let thread = self.id();
        if current_thread_id()? == thread {
            return set_current_thread_affinity(affinity);
        }
        validate_blocking_context()?;
        runtime_task_system()?
            .request_thread_affinity(thread, affinity)?
            .wait()
    }

    /// Looks up a generation-valid thread through the runtime-owned task system.
    pub fn lookup(thread: ThreadId) -> Result<Self, TaskError> {
        runtime_task_system()?.thread_handle(thread)
    }

    /// Returns a cumulative charged-runtime snapshot for a live thread.
    pub fn runtime(&self) -> Result<ThreadRuntimeSnapshot, TaskError> {
        let thread = self.id();
        runtime_task_system()?.thread_runtime(thread)
    }
}
