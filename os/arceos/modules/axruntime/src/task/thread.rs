use alloc::sync::Arc;
use core::sync::atomic::AtomicU8;

use super::*;

const THREAD_START_PENDING: u8 = 0;
const THREAD_START_ACTIVE: u8 = 1;
const THREAD_START_ABORTED: u8 = 2;

/// Scheduler thread whose resources and registry identity exist but which is
/// not yet runnable.
///
/// OS layers use this move-only transaction boundary to finish private resource
/// construction. They must call [`Self::stage`] before publishing externally
/// visible task identity, then call [`StagedThread::activate`] after committing
/// that identity. Dropping an unpublished value removes the scheduler record
/// and releases context, stack, TLS, extension, and address-space resources in
/// task context.
pub struct PreparedThread {
    system: &'static TaskSystem,
    handle: Option<ThreadHandle>,
    start: Arc<RuntimeThreadStart>,
}

/// Scheduler thread placed on a run queue but not yet activated by its OS.
///
/// This transaction token must be activated or dropped from task context.
/// Completing either path wakes the staged trampoline through a task-context
/// wait queue and is therefore not valid in a hard-interrupt handler.
#[must_use = "staged threads must be activated or explicitly dropped to abort"]
pub struct StagedThread {
    handle: Option<ThreadHandle>,
    start: Arc<RuntimeThreadStart>,
}

impl PreparedThread {
    /// Returns a strong handle for binding OS-owned identity before publication.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("prepared thread was already consumed")
            .clone()
    }

    /// Places and immediately activates a thread with no external publication
    /// transaction.
    pub fn publish(self) -> Result<ThreadHandle, TaskError> {
        Ok(self.stage()?.activate())
    }

    /// Completes the fallible scheduler placement phase without entering the
    /// caller-owned thread entry point.
    ///
    /// The scheduler may select the staged thread, but its runtime trampoline
    /// remains blocked on an internal start gate. This lets an OS complete its
    /// public identity transaction before [`StagedThread::activate`] provides
    /// the final infallible release, matching Linux's `wake_up_new_task`
    /// boundary.
    pub fn stage(mut self) -> Result<StagedThread, TaskError> {
        let handle = self
            .handle
            .take()
            .expect("prepared thread was already consumed");
        publish_prepared_thread(self.system, handle).map(|handle| StagedThread {
            handle: Some(handle),
            start: Arc::clone(&self.start),
        })
    }

    pub(super) fn new(
        system: &'static TaskSystem,
        handle: ThreadHandle,
        start: Arc<RuntimeThreadStart>,
    ) -> Self {
        Self {
            system,
            handle: Some(handle),
            start,
        }
    }
}

impl StagedThread {
    /// Returns a strong handle for the OS publication transaction.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("staged thread was already consumed")
            .clone()
    }

    /// Releases the staged thread to execute its caller-owned entry point.
    pub fn activate(mut self) -> ThreadHandle {
        let handle = self
            .handle
            .take()
            .expect("staged thread was already consumed");
        self.start.activate();
        handle
    }
}

impl Drop for StagedThread {
    fn drop(&mut self) {
        self.start.abort();
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

#[derive(Debug)]
pub(super) struct RuntimeThreadStart {
    state: AtomicU8,
    wait: WaitQueue,
}

impl RuntimeThreadStart {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(THREAD_START_PENDING),
            wait: WaitQueue::new(),
        }
    }

    fn activate(&self) {
        self.state
            .compare_exchange(
                THREAD_START_PENDING,
                THREAD_START_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .unwrap_or_else(|state| panic!("invalid staged-thread activation state: {state}"));
        self.wait.notify_all();
    }

    fn abort(&self) {
        if self
            .state
            .compare_exchange(
                THREAD_START_PENDING,
                THREAD_START_ABORTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.wait.notify_all();
        }
    }

    fn wait_for_activation(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            THREAD_START_ACTIVE => return true,
            THREAD_START_ABORTED => return false,
            THREAD_START_PENDING => {}
            state => panic!("invalid runtime thread-start state: {state}"),
        }
        self.wait
            .wait_until(|| self.state.load(Ordering::Acquire) != THREAD_START_PENDING);
        match self.state.load(Ordering::Acquire) {
            THREAD_START_ACTIVE => true,
            THREAD_START_ABORTED => false,
            state => panic!("invalid completed thread-start state: {state}"),
        }
    }
}

pub(super) struct RuntimeThreadData {
    pub(super) entry: SpinNoIrq<Option<KernelThreadEntry>>,
    pub(super) exit_code: AtomicI32,
    pub(super) exit_completed: AtomicBool,
    pub(super) join_wait: WaitQueue,
    pub(super) os_extension: Option<ThreadExtension>,
    pub(super) start: Arc<RuntimeThreadStart>,
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
        start: Arc<RuntimeThreadStart>,
    ) -> Self {
        Self {
            entry: SpinNoIrq::new(Some(entry)),
            exit_code: AtomicI32::new(0),
            exit_completed: AtomicBool::new(false),
            join_wait: WaitQueue::new(),
            os_extension,
            start,
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
            drop(error.into_retry_handle());
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
    if !data.start.wait_for_activation() {
        exit_current(0);
    }
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
