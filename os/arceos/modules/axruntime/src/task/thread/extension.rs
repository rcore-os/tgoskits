//! Runtime-owned scheduler extension composition and OS extension leases.

use super::*;

pub(in crate::task) static RUNTIME_THREAD_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: runtime_thread_switch_in_hook,
    on_switch_out: runtime_thread_switch_out_hook,
    on_exit: runtime_thread_exit_hook,
    on_deadline_overrun: runtime_thread_deadline_overrun_hook,
    drop: runtime_thread_drop_hook,
};

pub(in crate::task) unsafe fn runtime_thread_extension(data: usize) -> ThreadExtension {
    let os_extension = unsafe { runtime_thread_data_from_raw(data) }
        .os_extension
        .as_ref();
    let scheduler_tick_gate = os_extension.and_then(ThreadExtension::scheduler_tick_work_gate);
    let forwards_running_policy = os_extension
        .and_then(ThreadExtension::running_policy_applied_hook)
        .is_some();
    // SAFETY: the caller transfers one live `RuntimeThreadData` allocation
    // whose final destruction right belongs to this outer extension.
    let mut extension = unsafe { ThreadExtension::new(data, &RUNTIME_THREAD_EXTENSION_OPS) };
    if let Some(gate) = scheduler_tick_gate {
        // SAFETY: the outer callback retains `RuntimeThreadData` and forwards
        // exactly one generation-authorized publication, with the IRQ-observed
        // monotonic timestamp, to its inner extension.
        extension =
            unsafe { extension.with_scheduler_tick_work(gate, runtime_thread_scheduler_tick_hook) };
    }
    if forwards_running_policy {
        // SAFETY: the outer runtime extension owns the inner extension and
        // forwards the same running-owner base-policy observation without
        // retaining either borrowed data value.
        extension = unsafe {
            extension.with_running_policy_applied_hook(runtime_thread_policy_applied_hook)
        };
    }
    extension
}

unsafe extern "Rust" fn runtime_thread_switch_in_hook(
    data: usize,
    thread: ThreadId,
    base_policy: SchedulePolicy,
) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: `spawn_raw_with_extension` retains the OS extension until the
        // outer runtime extension is reaped and forwards the same thread ID.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread, base_policy) };
    }
}

unsafe extern "Rust" fn runtime_thread_switch_out_hook(
    data: usize,
    thread: ThreadId,
    reason: SwitchReason,
) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: same composition contract as `runtime_thread_switch_in_hook`.
        unsafe { (extension.ops().on_switch_out)(extension.data(), thread, reason) };
    }
}

unsafe extern "Rust" fn runtime_thread_exit_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: the TaskSystem invokes this in task context after committing exit.
        unsafe { (extension.ops().on_exit)(extension.data(), thread) };
    }
    // Runtime threads normally publish completion before their final schedule,
    // Linux-zombie style. Keep this idempotent fallback for externally marked
    // exits and failed-spawn cleanup paths that never ran the trampoline.
    publish_runtime_exit_completion(runtime);
}

pub(super) fn publish_runtime_exit_completion(runtime: &RuntimeThreadData) {
    if runtime
        .exit_completed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        runtime.join_wait.notify_all();
    }
}

unsafe extern "Rust" fn runtime_thread_deadline_overrun_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: the scheduler defers this callback to an ordinary safe point.
        unsafe { (extension.ops().on_deadline_overrun)(extension.data(), thread) };
    }
}

unsafe extern "Rust" fn runtime_thread_policy_applied_hook(
    data: usize,
    thread: ThreadId,
    base_policy: SchedulePolicy,
    observed_ns: u64,
) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    let Some(extension) = runtime.os_extension.as_ref() else {
        panic!(
            "runtime policy forwarding lost OS extension for thread {:#x}",
            thread.as_u64()
        );
    };
    if !unsafe { extension.forward_running_policy_applied(thread, base_policy, observed_ns) } {
        panic!(
            "runtime policy forwarding lost callback for thread {:#x}",
            thread.as_u64()
        );
    }
}

unsafe extern "Rust" fn runtime_thread_scheduler_tick_hook(
    data: usize,
    thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    let Some(extension) = runtime.os_extension.as_ref() else {
        panic!(
            "runtime scheduler-tick forwarding lost OS extension for thread {:#x}",
            thread.as_u64()
        );
    };
    unsafe { extension.forward_scheduler_tick_work(thread, observed_ns) }.unwrap_or_else(|| {
        panic!(
            "runtime scheduler-tick forwarding lost callback for thread {:#x}",
            thread.as_u64()
        )
    })
}

unsafe extern "Rust" fn runtime_thread_drop_hook(data: usize) {
    // SAFETY: the scheduler reaper invokes this exactly once for the pointer
    // transferred through `RUNTIME_THREAD_EXTENSION_OPS`.
    drop(unsafe { Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeThreadData>(data)) });
}

unsafe fn runtime_thread_data_from_raw(data: usize) -> &'static RuntimeThreadData {
    // SAFETY: every outer callback receives the Box pointer installed by
    // `spawn_raw_with_extension`, which remains valid until the drop callback.
    unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(data) }
}

/// Borrows the OS extension composed inside a runtime-owned thread record.
pub fn thread_os_extension(
    thread: &ThreadHandle,
) -> Result<Option<ThreadOsExtensionBorrow<'_>>, TaskError> {
    let runtime = task_system()
        .ok_or(TaskError::NotInitialized)?
        .thread_extension(thread)?;
    let RuntimeExtensionKind::Runtime = classify_runtime_extension(
        runtime.as_ref().map(|extension| extension.ops()),
        runtime.as_ref().map_or(0, |extension| extension.data()),
    )?
    else {
        return Ok(None);
    };
    let Some(runtime) = runtime else {
        unreachable!("classified runtime extension must be present")
    };
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and `runtime` borrows the strong caller handle for the whole result.
    let data = unsafe { runtime_thread_data_from_raw(runtime.data()) };
    Ok(data
        .os_extension
        .as_ref()
        .map(|extension| ThreadOsExtensionBorrow {
            data: extension.data(),
            ops: extension.ops(),
            _runtime: runtime,
        }))
}

/// Leases the current thread's composed OS extension.
pub fn current_os_extension() -> Result<Option<ThreadOsExtensionLease>, TaskError> {
    let runtime = current_thread_extension()?;
    let RuntimeExtensionKind::Runtime = classify_runtime_extension(
        runtime.as_ref().map(|extension| extension.ops()),
        runtime.as_ref().map_or(0, |extension| extension.data()),
    )?
    else {
        return Ok(None);
    };
    let Some(runtime) = runtime else {
        unreachable!("classified runtime extension must be present")
    };
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and the returned lease retains the outer scheduler extension lease.
    let data = unsafe { runtime_thread_data_from_raw(runtime.data()) };
    Ok(data
        .os_extension
        .as_ref()
        .map(|extension| ThreadOsExtensionLease {
            data: extension.data(),
            ops: extension.ops(),
            _runtime: runtime,
        }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::task) enum RuntimeExtensionKind {
    Missing,
    Runtime,
}

pub(in crate::task) fn classify_runtime_extension(
    ops: Option<&ThreadExtensionOps>,
    data: usize,
) -> Result<RuntimeExtensionKind, TaskError> {
    let Some(ops) = ops else {
        return Ok(RuntimeExtensionKind::Missing);
    };
    if !core::ptr::eq(ops, &RUNTIME_THREAD_EXTENSION_OPS) {
        return Err(TaskError::InvalidConfiguration);
    }
    if data == 0 || !data.is_multiple_of(core::mem::align_of::<RuntimeThreadData>()) {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(RuntimeExtensionKind::Runtime)
}
