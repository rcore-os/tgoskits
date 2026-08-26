//! Runtime thread entry, exit, completion, wait, and reap lifecycle.

use super::*;

/// Stores the exit code, marks the current thread exited, and switches away.
pub fn exit_current(exit_code: i32) -> ! {
    let current = current_thread_id()
        .unwrap_or_else(|error| panic!("failed to identify exiting runtime thread: {error}"));
    let primary = primary_bootstrap_thread()
        .unwrap_or_else(|| panic!("primary bootstrap thread identity is not initialized"));
    if primary == current {
        debug!("main task exited: exit_code={exit_code}");
        crate::terminate();
    }

    let exit_permit = ax_task::prepare_current_exit()
        .unwrap_or_else(|error| panic!("failed to prepare scheduler thread exit: {error}"));
    publish_current_runtime_exit(exit_code)
        .unwrap_or_else(|error| panic!("failed to publish thread exit: {error}"));
    ax_task::commit_current_exit(exit_permit)
}

/// Waits for a thread to finish executing without consuming its owning handle.
///
/// This split wait operation lets handle registries keep their raw-pointer or
/// map entry valid while the target still runs. Completion is published by the
/// exiting thread after its entry function and exit code are final, before the
/// non-returning scheduler exit. Physical off-CPU completion and final resource
/// reclamation are separate phases.
pub fn wait_thread(handle: &ThreadHandle) -> Result<i32, TaskError> {
    if current_thread_id()? == handle.id() {
        return Err(TaskError::InvalidConfiguration);
    }
    let data = runtime_thread_data(handle)?;
    data.join_wait
        .try_wait_until(|| data.exit_completed.load(Ordering::Acquire))?;
    Ok(data.exit_code.load(Ordering::Acquire))
}

/// Waits for an exited thread and returns its exit code.
///
/// Resource teardown is attempted synchronously once. A late IRQ wake or other
/// stable header reference may legitimately defer final reclamation, so join
/// releases its owning handle to the bounded task-system reaper instead of
/// spinning until unrelated references disappear.
pub fn join_thread(handle: ThreadHandle) -> Result<i32, TaskError> {
    let exit_code = wait_thread(&handle)?;
    match task_system()
        .ok_or(TaskError::NotInitialized)?
        .reap_thread_handle(handle)
    {
        Ok(()) => {}
        Err(error) => {
            let task_error = error.task_error();
            if !matches!(task_error, TaskError::ThreadBusy | TaskError::NotExited) {
                return Err(task_error);
            }
            drop(error.into_retry_handle());
        }
    }
    Ok(exit_code)
}

pub(in crate::task) unsafe extern "C" fn runtime_thread_entry() -> ! {
    finish_initial_scheduler_switch();
    let extension = ax_task::current_thread_extension()
        .unwrap_or_else(|error| panic!("kernel thread has no scheduler extension: {error}"))
        .unwrap_or_else(|| panic!("kernel thread entry is missing runtime data"));
    let data_raw = extension_data_after_releasing_lease(extension, &RUNTIME_THREAD_EXTENSION_OPS)
        .unwrap_or_else(|error| panic!("kernel thread extension type is invalid: {error}"));
    // SAFETY: the ops identity above proves the data pointer was created from
    // `Box<RuntimeThreadData>`. The registry record keeps it live through exit;
    // the temporary lease must not survive the non-unwinding exit path.
    let data = unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(data_raw) };
    if !data.start.wait_for_activation() {
        exit_current(0);
    }
    let entry = data
        .entry
        .lock_irqsave()
        .take()
        .unwrap_or_else(|| panic!("kernel thread entry was already consumed"));
    entry();
    exit_current(0)
}

pub(in crate::task) fn extension_data_after_releasing_lease(
    extension: ax_task::ThreadExtensionLease,
    expected_ops: &'static ThreadExtensionOps,
) -> Result<usize, TaskError> {
    if !core::ptr::eq(extension.ops(), expected_ops) {
        return Err(TaskError::InvalidConfiguration);
    }
    let extension = unsafe {
        // SAFETY: the runtime calls this only from the leased running thread's
        // entry trampoline, and its registry record remains live through exit.
        extension.release_for_current_thread_entry()
    };
    Ok(extension.data())
}

pub(in crate::task) fn finish_initial_scheduler_switch() {
    // SAFETY: both architecture entry trampolines invoke this exactly once as
    // their first operation after inheriting the scheduler IRQ-guard baton.
    unsafe { ax_task::finish_initial_context_switch() }
        .unwrap_or_else(|error| panic!("failed to complete initial context switch: {error}"));
}

pub(in crate::task) unsafe fn release_transferred_extension(extension: Option<ThreadExtension>) {
    drop(extension);
}

pub(in crate::task) fn runtime_thread_data(
    thread: &ThreadHandle,
) -> Result<&RuntimeThreadData, TaskError> {
    let extension = task_system()
        .ok_or(TaskError::NotInitialized)?
        .thread_extension(thread)?
        .ok_or(TaskError::InvalidConfiguration)?;
    if !core::ptr::eq(extension.ops(), &RUNTIME_THREAD_EXTENSION_OPS) {
        return Err(TaskError::InvalidConfiguration);
    }
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and the returned reference is bounded by the strong caller handle.
    Ok(unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(extension.data()) })
}

fn publish_current_runtime_exit(exit_code: i32) -> Result<(), TaskError> {
    let thread = current_thread_handle()?;
    let data = runtime_thread_data(&thread)?;
    data.exit_code.store(exit_code, Ordering::Release);
    super::extension::publish_runtime_exit_completion(data);
    Ok(())
}
