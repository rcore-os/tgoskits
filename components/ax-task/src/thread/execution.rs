//! Publication tokens and task-owned execution/completion state.
use alloc::{boxed::Box, string::String};
use core::{
    fmt,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

use crate::{
    runtime::{
        context::runtime_task_system,
        delivery::inbox::{InboxKind, InboxNode},
        lock::PreemptTicketLock,
        task_runtime,
    },
    sync::WaitQueue,
    thread::{TaskError, ThreadHandle},
};

/// Owns a new task which has never entered a runqueue.
///
/// Drop queues cancellation for the task-context reaper, including in hard IRQ.
/// Retain a thread handle and join it to observe cancellation completion.
#[must_use = "prepare must be published or cancelled"]
pub struct PreparedThread {
    handle: Option<ThreadHandle>,
}
impl PreparedThread {
    pub(crate) fn new(handle: ThreadHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }
    /// Borrows task identity for OS resource initialization.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("unconsumed preparation")
            .clone()
    }
    /// Reserves initial placement without making the task runnable.
    pub fn stage(mut self) -> Result<StagedThread, TaskError> {
        let handle = self.handle.as_ref().expect("unconsumed preparation");
        runtime_task_system()?.stage_new_thread(handle)?;
        Ok(StagedThread {
            handle: self.handle.take(),
        })
    }
    /// Activates a task without an external identity publication transaction.
    pub fn publish(self) -> Result<ThreadHandle, TaskError> {
        Ok(self.stage()?.activate())
    }
}
impl Drop for PreparedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            cancel_new_thread(handle);
        }
    }
}

/// Owns a reserved first activation, analogous to Linux's pre-wake TASK_NEW.
///
/// Drop queues cancellation; the reaper releases the reservation and entry.
/// The task never becomes runnable when cancelled.
#[must_use = "staged publication must be activated or cancelled"]
pub struct StagedThread {
    handle: Option<ThreadHandle>,
}
impl StagedThread {
    /// Borrows task identity while the OS publishes its resources.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle.as_ref().expect("unconsumed stage").clone()
    }
    /// Commits first activation after all external identity publication is complete.
    pub fn activate(mut self) -> ThreadHandle {
        let mut irq = crate::runtime::context::RuntimeIrqGuard::enter();
        let mut cpu = crate::runtime::context::runtime_current_cpu_mut(&mut irq)
            .expect("activation requires an installed owner CPU");
        let handle = self.handle.as_ref().expect("unconsumed stage");
        runtime_task_system()
            .expect("staged system remains installed")
            .activate_staged_thread(cpu.as_mut(), handle);
        // Until admission commits, Drop must retain cancellation ownership.
        // Consume it before dropping IRQ/preemption guards, which may schedule.
        self.handle.take().expect("committed stage")
    }
}
impl Drop for StagedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            cancel_new_thread(handle);
        }
    }
}

pub(crate) struct ThreadExecution {
    pub(crate) cancellation_node: InboxNode,
    entry: PreemptTicketLock<Option<Box<dyn FnOnce() + Send + 'static>>>,
    completion: WaitQueue,
    completed: AtomicBool,
    exit_code: AtomicI32,
    name: String,
}
impl fmt::Debug for ThreadExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadExecution")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
impl ThreadExecution {
    pub(crate) fn new(entry: Box<dyn FnOnce() + Send + 'static>, name: String) -> Self {
        Self {
            cancellation_node: InboxNode::new(InboxKind::Reclaim),
            entry: PreemptTicketLock::new(Some(entry)),
            completion: WaitQueue::new(),
            completed: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            name,
        }
    }
    pub(crate) fn finish(&self) {
        // A cancelled TASK_NEW never consumes its entry. Dispose its captures
        // in task context before publishing completion, outside the entry lock.
        let unused_entry = self.entry.lock().take();
        drop(unused_entry);
        if !self.completed.swap(true, Ordering::AcqRel) {
            self.completion.notify_all();
        }
    }
}

impl ThreadHandle {
    /// Borrows the directly attached OS extension for the lifetime of this handle.
    pub fn extension(&self) -> Option<crate::thread::ThreadExtensionBorrow<'_>> {
        self.extension_view()
            .map(|view| crate::thread::ThreadExtensionBorrow::new(view, self))
    }

    /// Waits for logical exit while retaining the task identity and OS extension.
    pub fn wait(&self) -> Result<i32, TaskError> {
        if crate::thread::current::current_thread_id()? == self.id() {
            return Err(TaskError::InvalidConfiguration);
        }
        let execution = self
            .core
            .execution
            .as_ref()
            .ok_or(TaskError::InvalidConfiguration)?;
        execution
            .completion
            .try_wait_until(|| execution.completed.load(Ordering::Acquire))?;
        Ok(execution.exit_code.load(Ordering::Acquire))
    }
    /// Waits for exit and transfers final reclamation to the task-context reaper.
    pub fn join(self) -> Result<i32, TaskError> {
        let code = self.wait()?;
        match runtime_task_system()?.reap_thread_handle(self) {
            Ok(()) => (),
            Err(error)
                if matches!(
                    error.task_error(),
                    TaskError::ThreadBusy | TaskError::NotExited
                ) =>
            {
                drop(error.into_retry_handle())
            }
            Err(error) => return Err(error.task_error()),
        }
        Ok(code)
    }
    /// Relinquishes the caller's management lease; the scheduler owns execution.
    pub fn detach(self) {
        drop(self);
    }
}

/// Publishes a return code and exits through the one scheduler exit transaction.
pub fn exit_current(exit_code: i32) -> ! {
    let permit = crate::thread::current::prepare_current_exit()
        .unwrap_or_else(|error| task_runtime::fatal_invariant(15, error_code(error)));
    let core = crate::thread::current::current_thread_core_arc()
        .unwrap_or_else(|error| task_runtime::fatal_invariant(10, error_code(error)));
    if let Some(execution) = core.execution.as_ref() {
        execution.exit_code.store(exit_code, Ordering::Relaxed);
        execution.finish();
    }
    drop(core);
    crate::thread::current::commit_current_exit(permit)
}

pub(crate) unsafe extern "C" fn thread_entry() -> ! {
    // SAFETY: a fresh architecture context transfers exactly one switch baton.
    unsafe { crate::runtime::switch::finish_initial_context_switch() }
        .unwrap_or_else(|error| task_runtime::fatal_invariant(9, error_code(error)));
    let core = crate::thread::current::current_thread_core_arc()
        .unwrap_or_else(|error| task_runtime::fatal_invariant(10, error_code(error)));
    let entry = core
        .execution
        .as_ref()
        .expect("thread entry requires execution state")
        .entry
        .lock()
        .take()
        .expect("thread entry runs once");
    // The entry may exit without unwinding. Do not retain an owning reference on its stack.
    drop(core);
    entry();
    exit_current(0)
}

fn cancel_new_thread(handle: ThreadHandle) {
    let system = runtime_task_system().expect("prepared task system remains installed");
    system.publish_thread_cancellation(&handle.core);
    drop(handle);
}

const fn error_code(error: TaskError) -> usize {
    match error {
        TaskError::NotInitialized => 1,
        TaskError::InvalidRuntimeHandle => 2,
        TaskError::NoRunnableThread => 3,
        TaskError::UnsafeContext => 4,
        _ => 255,
    }
}
