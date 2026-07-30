use super::*;

/// Scheduler thread whose resources and registry identity exist but which is
/// not yet runnable.
///
/// OS layers use this move-only transaction boundary to publish their own task,
/// PID, and resource registries before the new context can execute. Dropping an
/// unpublished value removes the scheduler record and releases context, stack,
/// TLS, extension, and address-space resources in task context.
pub struct PreparedThread {
    system: &'static TaskSystem,
    handle: Option<ThreadHandle>,
}

impl PreparedThread {
    /// Returns a strong handle for binding OS-owned identity before publication.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("prepared thread was already consumed")
            .clone()
    }

    /// Makes the prepared thread ready and places it on a run queue.
    pub fn publish(mut self) -> Result<ThreadHandle, TaskError> {
        let handle = self
            .handle
            .take()
            .expect("prepared thread was already consumed");
        publish_prepared_thread(self.system, handle)
    }

    pub(super) fn new(system: &'static TaskSystem, handle: ThreadHandle) -> Self {
        Self {
            system,
            handle: Some(handle),
        }
    }
}

impl Drop for PreparedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            cleanup_failed_thread(self.system, handle);
        }
    }
}

type KernelThreadEntry = Box<dyn FnOnce() + Send + 'static>;

pub(super) struct RuntimeThreadData {
    pub(super) entry: SpinNoIrq<Option<KernelThreadEntry>>,
    pub(super) exit_code: AtomicI32,
    pub(super) exit_completed: AtomicBool,
    pub(super) join_wait: WaitQueue,
    pub(super) os_extension: Option<ThreadExtension>,
    pub(super) _name: String,
}

/// OS extension borrowed through the runtime's outer scheduler extension.
#[derive(Debug)]
pub struct ThreadOsExtensionBorrow<'thread> {
    _runtime: ax_task::ThreadExtensionBorrow<'thread>,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionBorrow<'_> {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

/// OS extension lease for current-thread lookups without an existing handle.
#[derive(Debug)]
pub struct ThreadOsExtensionLease {
    _runtime: ax_task::ThreadExtensionLease,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionLease {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

impl RuntimeThreadData {
    pub(super) fn new(
        entry: KernelThreadEntry,
        name: String,
        os_extension: Option<ThreadExtension>,
    ) -> Self {
        Self {
            entry: SpinNoIrq::new(Some(entry)),
            exit_code: AtomicI32::new(0),
            exit_completed: AtomicBool::new(false),
            join_wait: WaitQueue::new(),
            os_extension,
            _name: name,
        }
    }
}

pub(super) static RUNTIME_THREAD_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: runtime_thread_switch_in_hook,
    on_switch_out: runtime_thread_switch_out_hook,
    on_exit: runtime_thread_exit_hook,
    on_deadline_overrun: runtime_thread_deadline_overrun_hook,
    drop: runtime_thread_drop_hook,
};

pub(super) unsafe fn runtime_thread_extension(data: usize) -> ThreadExtension {
    let scheduler_tick_gate = unsafe { runtime_thread_data_from_raw(data) }
        .os_extension
        .as_ref()
        .and_then(ThreadExtension::scheduler_tick_work_gate);
    // SAFETY: the caller transfers one live `RuntimeThreadData` allocation
    // whose final destruction right belongs to this outer extension.
    let extension = unsafe { ThreadExtension::new(data, &RUNTIME_THREAD_EXTENSION_OPS) };
    if let Some(gate) = scheduler_tick_gate {
        // SAFETY: the outer callback retains `RuntimeThreadData`, then forwards
        // exactly one generation-authorized publication to its inner extension.
        unsafe { extension.with_scheduler_tick_work(gate, runtime_thread_scheduler_tick_hook) }
    } else {
        extension
    }
}

unsafe extern "Rust" fn runtime_thread_switch_in_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: `spawn_raw_with_extension` retains the OS extension until the
        // outer runtime extension is reaped and forwards the same thread ID.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread) };
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

fn publish_runtime_exit_completion(runtime: &RuntimeThreadData) {
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

unsafe extern "Rust" fn runtime_thread_scheduler_tick_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    let Some(extension) = runtime.os_extension.as_ref() else {
        panic!(
            "runtime scheduler-tick forwarding lost OS extension for thread {:#x}",
            thread.as_u64()
        );
    };
    let forwarded = unsafe { extension.forward_scheduler_tick_work(thread) };
    if !forwarded {
        panic!(
            "runtime scheduler-tick forwarding lost callback for thread {:#x}",
            thread.as_u64()
        );
    }
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

/// Stores the exit code, marks the current thread exited, and switches away.
pub fn exit_current(exit_code: i32) -> ! {
    let current = current_thread_id()
        .unwrap_or_else(|error| panic!("failed to identify exiting runtime thread: {error}"));
    let primary = primary_bootstrap_thread()
        .unwrap_or_else(|| panic!("primary bootstrap thread identity is not initialized"));
    if primary == current {
        debug!("main task exited: exit_code={exit_code}");
        let _irq = IrqSave::new();
        #[cfg(feature = "irq")]
        crate::clock_event_runtime::take_current_clock_event_offline();
        ax_hal::power::system_off();
    }

    let exit_permit = ax_task::prepare_current_exit()
        .unwrap_or_else(|error| panic!("failed to prepare scheduler thread exit: {error}"));
    publish_current_runtime_exit(exit_code)
        .unwrap_or_else(|error| panic!("failed to publish thread exit: {error}"));
    ax_task::commit_current_exit(exit_permit)
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
pub(super) enum RuntimeExtensionKind {
    Missing,
    Runtime,
}

pub(super) fn classify_runtime_extension(
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
            drop(
                error
                    .into_retry_handle()
                    .expect("busy owned reap must return its handle"),
            );
        }
    }
    Ok(exit_code)
}

pub(super) unsafe extern "C" fn runtime_thread_entry() -> ! {
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
    let entry = data
        .entry
        .lock()
        .take()
        .unwrap_or_else(|| panic!("kernel thread entry was already consumed"));
    entry();
    exit_current(0)
}

pub(super) fn extension_data_after_releasing_lease(
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

pub(super) fn finish_initial_scheduler_switch() {
    // SAFETY: both architecture entry trampolines invoke this exactly once as
    // their first operation after inheriting the scheduler IRQ-guard baton.
    unsafe { ax_task::finish_initial_context_switch() }
        .unwrap_or_else(|error| panic!("failed to complete initial context switch: {error}"));
}

pub(super) unsafe fn release_transferred_extension(extension: Option<ThreadExtension>) {
    drop(extension);
}

pub(super) fn runtime_thread_data(thread: &ThreadHandle) -> Result<&RuntimeThreadData, TaskError> {
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
    publish_runtime_exit_completion(data);
    Ok(())
}

fn publish_prepared_thread(
    system: &'static TaskSystem,
    handle: ThreadHandle,
) -> Result<ThreadHandle, TaskError> {
    let result = system.make_ready(handle.id()).and_then(|()| {
        with_current_cpu_local_mut_owner(|cpu| {
            system.place_ready(cpu, handle.id(), ax_hal::time::monotonic_time_nanos())
        })
    });
    if let Err(error) = result {
        cleanup_failed_thread(system, handle);
        return Err(error);
    }
    Ok(handle)
}

fn cleanup_failed_thread(system: &TaskSystem, handle: ThreadHandle) {
    let thread = handle.id();
    let _ = system.mark_exited(thread);
    drop(handle);
    let _ = system.reap_thread(thread);
}
