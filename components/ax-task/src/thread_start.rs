//! Runtime-backed construction and ownership of portable kernel threads.

use alloc::{boxed::Box, string::String};
use core::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    CpuSet, SchedulePolicy, SwitchReason, TaskError, ThreadExtension, ThreadExtensionOps,
    ThreadHandle, ThreadId, ThreadResources, ThreadSpec, WaitQueue,
    facade::{RuntimeIrqGuard, runtime_current_cpu_mut, runtime_task_system},
    lock::IrqTicketLock,
    runtime::{
        AddressSpaceHandle, ExecutionContextHandle, KernelContextRequest, RuntimeStatus,
        StackHandle, StackRequest, TlsHandle, TlsRequest, task_runtime,
    },
};

/// Default stack size used by portable kernel service threads.
pub const DEFAULT_KERNEL_THREAD_STACK_SIZE: usize = 256 * 1024;

/// Resource and diagnostic configuration for one kernel thread.
#[derive(Debug)]
pub struct KernelThreadSpec {
    name: String,
    stack_size: usize,
    stack_alignment: usize,
    guard_size: usize,
    policy: SchedulePolicy,
    affinity: Option<CpuSet>,
    os_extension: Option<ThreadExtension>,
}

/// Builder for one portable kernel service thread.
#[derive(Debug)]
pub struct ThreadBuilder {
    spec: KernelThreadSpec,
}

impl ThreadBuilder {
    /// Starts a builder with default portable stack requirements.
    pub fn new(name: String) -> Self {
        Self {
            spec: KernelThreadSpec::new(name),
        }
    }

    /// Selects the usable stack size in bytes.
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.spec = self.spec.with_stack_size(stack_size);
        self
    }

    /// Selects the stack alignment in bytes.
    pub fn stack_alignment(mut self, stack_alignment: usize) -> Self {
        self.spec = self.spec.with_stack_alignment(stack_alignment);
        self
    }

    /// Requests an inaccessible stack guard area from the runtime.
    pub fn guard_size(mut self, guard_size: usize) -> Self {
        self.spec = self.spec.with_guard_size(guard_size);
        self
    }

    /// Selects the base scheduler policy.
    pub fn policy(mut self, policy: SchedulePolicy) -> Self {
        self.spec = self.spec.with_policy(policy);
        self
    }

    /// Restricts placement to the supplied topology-sized CPU set.
    pub fn affinity(mut self, affinity: CpuSet) -> Self {
        self.spec = self.spec.with_affinity(affinity);
        self
    }

    /// Composes one OS-owned extension inside the portable thread wrapper.
    ///
    /// # Safety
    ///
    /// `extension` transfers unique callback-data ownership into this builder.
    /// The caller must not install another copy or invoke its drop callback.
    /// This builder must be spawned or dropped in ordinary task context.
    pub unsafe fn extension(mut self, extension: ThreadExtension) -> Self {
        self.spec = unsafe {
            // SAFETY: this method forwards its unique-ownership contract.
            self.spec.with_extension(extension)
        };
        self
    }

    /// Allocates, creates, and enqueues the configured thread.
    ///
    /// # Errors
    ///
    /// Returns scheduler validation or runtime resource errors from
    /// [`spawn_kernel_thread`].
    pub fn spawn<F>(self, entry: F) -> Result<KernelThreadHandle, TaskError>
    where
        F: FnOnce() + Send + 'static,
    {
        spawn_kernel_thread(self.spec, entry)
    }
}

/// Join capability for one runtime-backed kernel thread.
///
/// Callers must either [`join`](Self::join) a thread that may return or mark a
/// shutdown-lifetime worker with [`detach_permanent`](Self::detach_permanent).
#[derive(Debug)]
#[must_use = "kernel threads must be joined or explicitly detached as permanent"]
pub struct KernelThreadHandle {
    thread: Option<ThreadHandle>,
}

impl KernelThreadHandle {
    /// Returns the scheduler identity of this kernel thread.
    pub fn id(&self) -> ThreadId {
        self.thread
            .as_ref()
            .expect("kernel thread handle is consumed only by ownership methods")
            .id()
    }

    /// Waits for logical thread exit and hands reclamation to the bounded reaper.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::InvalidConfiguration`] when joining the current
    /// thread and propagates scheduler wait or resource teardown errors.
    pub fn join(mut self) -> Result<(), TaskError> {
        let handle = self.thread.take().ok_or(TaskError::InvalidConfiguration)?;
        if crate::current_thread_id()? == handle.id() {
            return Err(TaskError::InvalidConfiguration);
        }
        let data = kernel_thread_data(&handle)?;
        data.join_wait
            .try_wait_until(|| data.exit_completed.load(Ordering::Acquire))?;
        reap_joined_thread(handle)
    }

    /// Marks a worker as intentionally live until scheduler shutdown.
    ///
    /// The entry closure must never return. Shutdown owns the remaining registry
    /// record and runtime resources; this method performs no hidden reaping.
    pub fn detach_permanent(mut self) {
        let _thread = self.thread.take();
    }
}

fn reap_joined_thread(mut handle: ThreadHandle) -> Result<(), TaskError> {
    match runtime_task_system()?.reap_thread_handle(handle) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.task_error(),
                TaskError::ThreadBusy | TaskError::NotExited
            ) =>
        {
            handle = error
                .into_retry_handle()
                .expect("retryable owned reap must return its handle");
            drop(handle);
            Ok(())
        }
        Err(error) => Err(error.task_error()),
    }
}

impl KernelThreadSpec {
    /// Creates a specification with a 256 KiB stack and no required guard area.
    pub fn new(name: String) -> Self {
        Self {
            name,
            stack_size: DEFAULT_KERNEL_THREAD_STACK_SIZE,
            stack_alignment: 16,
            guard_size: 0,
            policy: SchedulePolicy::default(),
            affinity: None,
            os_extension: None,
        }
    }

    /// Selects the usable stack size in bytes.
    pub const fn with_stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = stack_size;
        self
    }

    /// Selects the stack alignment in bytes.
    pub const fn with_stack_alignment(mut self, stack_alignment: usize) -> Self {
        self.stack_alignment = stack_alignment;
        self
    }

    /// Requests an inaccessible stack guard area from the runtime.
    pub const fn with_guard_size(mut self, guard_size: usize) -> Self {
        self.guard_size = guard_size;
        self
    }

    /// Selects the base scheduler policy.
    pub const fn with_policy(mut self, policy: SchedulePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Restricts placement to the supplied topology-sized CPU set.
    pub fn with_affinity(mut self, affinity: CpuSet) -> Self {
        self.affinity = Some(affinity);
        self
    }

    /// Composes one OS-owned extension inside the portable thread wrapper.
    ///
    /// # Safety
    ///
    /// `extension` transfers unique callback-data ownership into this spec. The
    /// caller must not install another copy or invoke its drop callback. This
    /// spec must be consumed or dropped in ordinary task context.
    pub unsafe fn with_extension(mut self, extension: ThreadExtension) -> Self {
        self.os_extension = Some(extension);
        self
    }

    fn stack_request(&self) -> StackRequest {
        StackRequest {
            usable_size: self.stack_size,
            alignment: self.stack_alignment,
            guard_size: self.guard_size,
        }
    }
}

/// Creates and enqueues a joinable kernel service thread.
///
/// The closure remains inside ax-task-owned extension data. Only opaque stack,
/// TLS, and context handles cross [`crate::runtime::TaskRuntime`].
///
/// # Errors
///
/// Returns [`TaskError::NotInitialized`] before the runtime publishes scheduler
/// objects, [`TaskError::InvalidConfiguration`] for invalid stack requirements,
/// and [`TaskError::RuntimeFailure`] when a runtime resource operation fails.
pub fn spawn_kernel_thread<F>(
    mut spec: KernelThreadSpec,
    entry: F,
) -> Result<KernelThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    validate_spec(&spec)?;
    let system = runtime_task_system()?;
    let resources = allocate_thread_resources(spec.stack_request())?;
    let extension_data = Box::into_raw(Box::new(KernelThreadData::new(
        entry,
        core::mem::take(&mut spec.name),
        spec.os_extension.take(),
    )))
    .expose_provenance();
    // SAFETY: the boxed data remains live until the scheduler reaper invokes
    // `kernel_thread_drop` through this exact callback-table identity.
    let extension = unsafe { ThreadExtension::new(extension_data, &KERNEL_THREAD_OPS) };
    let mut thread_spec = unsafe {
        // SAFETY: allocation above created one live, uniquely owned resource
        // bundle and this specification is its sole installation path.
        ThreadSpec::new(spec.policy)
            .with_extension(extension)
            .with_resources(resources)
    };
    if let Some(affinity) = spec.affinity.take() {
        thread_spec = thread_spec.with_affinity(affinity);
    }
    let handle = system.create_thread(thread_spec)?;

    let mut irq_guard = RuntimeIrqGuard::enter();
    let now_ns = task_runtime::monotonic_ns();
    let result = runtime_current_cpu_mut(&mut irq_guard).and_then(|mut cpu| {
        system.make_ready(handle.id())?;
        system.place_ready(cpu.as_mut(), handle.id(), now_ns)
    });
    drop(irq_guard);
    if let Err(error) = result {
        cleanup_unstarted_thread(system, handle);
        return Err(error);
    }
    Ok(KernelThreadHandle {
        thread: Some(handle),
    })
}

type KernelThreadEntry = Box<dyn FnOnce() + Send + 'static>;

struct KernelThreadData {
    entry: IrqTicketLock<Option<KernelThreadEntry>>,
    join_wait: WaitQueue,
    exit_completed: AtomicBool,
    os_extension: Option<ThreadExtension>,
    _name: String,
}

impl KernelThreadData {
    fn new(
        entry: impl FnOnce() + Send + 'static,
        name: String,
        os_extension: Option<ThreadExtension>,
    ) -> Self {
        Self {
            entry: IrqTicketLock::new(Some(Box::new(entry))),
            join_wait: WaitQueue::new(),
            exit_completed: AtomicBool::new(false),
            os_extension,
            _name: name,
        }
    }
}

static KERNEL_THREAD_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: kernel_thread_switch_in,
    on_switch_out: kernel_thread_switch_out,
    on_exit: kernel_thread_exit,
    on_deadline_overrun: kernel_thread_deadline_overrun,
    drop: kernel_thread_drop,
};

unsafe extern "Rust" fn kernel_thread_switch_in(data: usize, thread: ThreadId) {
    let data = unsafe { kernel_thread_data_from_raw(data) };
    if let Some(extension) = data.os_extension.as_ref() {
        // SAFETY: the outer extension owns and forwards the inner callback.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread) };
    }
}

unsafe extern "Rust" fn kernel_thread_switch_out(
    data: usize,
    thread: ThreadId,
    reason: SwitchReason,
) {
    let data = unsafe { kernel_thread_data_from_raw(data) };
    if let Some(extension) = data.os_extension.as_ref() {
        // SAFETY: the outer extension owns and forwards the inner callback.
        unsafe { (extension.ops().on_switch_out)(extension.data(), thread, reason) };
    }
}

unsafe extern "Rust" fn kernel_thread_exit(data: usize, thread: ThreadId) {
    let data = unsafe { kernel_thread_data_from_raw(data) };
    if let Some(extension) = data.os_extension.as_ref() {
        // SAFETY: exit is already deferred to ordinary task context.
        unsafe { (extension.ops().on_exit)(extension.data(), thread) };
    }
    publish_kernel_thread_exit_completion(data);
}

fn publish_kernel_thread_exit_completion(data: &KernelThreadData) {
    if data
        .exit_completed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        data.join_wait.notify_all();
    }
}

unsafe extern "Rust" fn kernel_thread_deadline_overrun(data: usize, thread: ThreadId) {
    let data = unsafe { kernel_thread_data_from_raw(data) };
    if let Some(extension) = data.os_extension.as_ref() {
        // SAFETY: Deadline notification runs at a scheduler safe point.
        unsafe { (extension.ops().on_deadline_overrun)(extension.data(), thread) };
    }
}

unsafe extern "Rust" fn kernel_thread_drop(data: usize) {
    // SAFETY: the extension owns the unique Box pointer until this callback.
    drop(unsafe { Box::from_raw(ptr::with_exposed_provenance_mut::<KernelThreadData>(data)) });
}

unsafe fn kernel_thread_data_from_raw(data: usize) -> &'static KernelThreadData {
    // SAFETY: every outer callback receives the live Box pointer installed with
    // KERNEL_THREAD_OPS, which remains valid until its drop callback.
    unsafe { &*ptr::with_exposed_provenance::<KernelThreadData>(data) }
}

unsafe extern "C" fn kernel_thread_entry() -> ! {
    if let Err(error) = unsafe {
        // SAFETY: this is the first operation in a fresh runtime context, which
        // inherits exactly one scheduler switch guard and consumes it once.
        crate::finish_initial_context_switch()
    } {
        task_runtime::fatal_invariant(9, error_code(error));
    }
    let extension = crate::current_thread_extension()
        .unwrap_or_else(|error| task_runtime::fatal_invariant(10, error_code(error)))
        .unwrap_or_else(|| task_runtime::fatal_invariant(11, 0));
    if !core::ptr::eq(extension.ops(), &KERNEL_THREAD_OPS) {
        task_runtime::fatal_invariant(12, extension.data());
    }
    let extension = unsafe {
        // SAFETY: this trampoline is the running thread named by the lease;
        // its registry record remains live until the non-returning exit below.
        extension.release_for_current_thread_entry()
    };
    let data_raw = extension.data();
    // SAFETY: the checked callback-table identity belongs only to
    // `KernelThreadData`. The registry record retains the extension while the
    // current thread runs, so the entry trampoline must release its temporary
    // lease before entering a function that exits without unwinding.
    let data = unsafe { &*ptr::with_exposed_provenance::<KernelThreadData>(data_raw) };
    let Some(entry) = data.entry.lock().take() else {
        task_runtime::fatal_invariant(13, data_raw);
    };
    entry();
    let exit_permit = crate::prepare_current_exit()
        .unwrap_or_else(|error| task_runtime::fatal_invariant(15, error_code(error)));
    // Logical completion is observable before the final non-returning
    // schedule-out only after every recoverable scheduler precondition has
    // been validated. Registry state, `on_cpu`, and the exit callback continue
    // to gate physical reclamation independently.
    publish_kernel_thread_exit_completion(data);
    crate::commit_current_exit(exit_permit)
}

fn validate_spec(spec: &KernelThreadSpec) -> Result<(), TaskError> {
    if spec.stack_size == 0 || spec.stack_alignment == 0 || !spec.stack_alignment.is_power_of_two()
    {
        Err(TaskError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

fn kernel_thread_data(handle: &ThreadHandle) -> Result<&KernelThreadData, TaskError> {
    let extension = runtime_task_system()?
        .thread_extension(handle)?
        .ok_or(TaskError::InvalidConfiguration)?;
    if !core::ptr::eq(extension.ops(), &KERNEL_THREAD_OPS) {
        return Err(TaskError::InvalidConfiguration);
    }
    // SAFETY: the checked ops identity belongs only to KernelThreadData, and
    // the returned borrow is bounded by `handle`, which keeps the registry
    // record live until the caller is finished with the data.
    Ok(unsafe { &*ptr::with_exposed_provenance::<KernelThreadData>(extension.data()) })
}

fn allocate_thread_resources(request: StackRequest) -> Result<ThreadResources, TaskError> {
    let stack_result = task_runtime::allocate_stack(request);
    if stack_result.status != RuntimeStatus::Success {
        return Err(runtime_error(stack_result.status));
    }
    if stack_result.handle == 0 {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    // SAFETY: successful TaskRuntime stack allocation returns one non-zero,
    // uniquely owned handle that remains live until deallocation.
    let stack = unsafe { StackHandle::from_raw(stack_result.handle) };
    let tls_result = task_runtime::allocate_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => {
            let _status = task_runtime::deallocate_stack(stack);
            return Err(TaskError::InvalidRuntimeHandle);
        }
        (RuntimeStatus::Success, handle) => {
            // SAFETY: successful TaskRuntime TLS allocation returns one
            // non-zero, uniquely owned handle live until deallocation.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => {
            let _status = task_runtime::deallocate_stack(stack);
            return Err(runtime_error(status));
        }
    };
    let context_result = task_runtime::create_kernel_context(KernelContextRequest {
        stack,
        entry: kernel_thread_entry,
        tls,
        address_space: AddressSpaceHandle::NONE,
    });
    if context_result.status != RuntimeStatus::Success {
        if !tls.is_none() {
            let _status = task_runtime::deallocate_tls(tls);
        }
        let _status = task_runtime::deallocate_stack(stack);
        return Err(runtime_error(context_result.status));
    }
    if context_result.handle == 0 {
        if !tls.is_none() {
            let _status = task_runtime::deallocate_tls(tls);
        }
        let _status = task_runtime::deallocate_stack(stack);
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(unsafe {
        // SAFETY: all handles were just created by the active runtime and their
        // unique destruction rights move into the returned bundle.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context_result.handle),
            stack,
            tls,
            AddressSpaceHandle::NONE,
        )
    })
}

fn cleanup_unstarted_thread(system: &crate::TaskSystem, handle: ThreadHandle) {
    let thread = handle.id();
    let _result = system.mark_exited(thread);
    drop(handle);
    let _result = system.reap_thread(thread);
}

const fn runtime_error(status: RuntimeStatus) -> TaskError {
    TaskError::RuntimeFailure(status as u32)
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: test_extension_hook,
        on_switch_out: test_extension_switch_out,
        on_exit: test_extension_hook,
        on_deadline_overrun: test_extension_hook,
        drop: test_extension_drop,
    };

    #[test]
    fn dropping_unspawned_builder_releases_owned_extension() {
        let drops = AtomicUsize::new(0);
        let extension = unsafe {
            // SAFETY: the builder is dropped synchronously while `drops` lives.
            ThreadExtension::new(
                (&drops as *const AtomicUsize).expose_provenance(),
                &TEST_EXTENSION_OPS,
            )
        };
        let builder = unsafe {
            // SAFETY: this test transfers the sole callback ownership.
            ThreadBuilder::new(String::from("drop-test")).extension(extension)
        };

        drop(builder);

        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn invalid_spec_releases_extension_before_runtime_lookup() {
        let drops = AtomicUsize::new(0);
        let extension = unsafe {
            // SAFETY: invalid-spec validation drops the extension synchronously.
            ThreadExtension::new(
                (&drops as *const AtomicUsize).expose_provenance(),
                &TEST_EXTENSION_OPS,
            )
        };
        let spec = unsafe {
            // SAFETY: this test transfers the sole callback ownership.
            KernelThreadSpec::new(String::from("invalid-test"))
                .with_stack_size(0)
                .with_extension(extension)
        };

        let result = spawn_kernel_thread(spec, || {});

        assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    unsafe extern "Rust" fn test_extension_hook(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn test_extension_switch_out(
        _data: usize,
        _thread: ThreadId,
        _reason: SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn test_extension_drop(data: usize) {
        // SAFETY: each test supplies a live AtomicUsize for the synchronous drop.
        let drops = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
        drops.fetch_add(1, Ordering::AcqRel);
    }
}
