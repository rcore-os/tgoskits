//! Common thread construction, publication, completion, and join ownership.

use alloc::string::String;

use crate::{
    runtime::{
        RuntimeStatus,
        context::runtime_task_system,
        resource::{
            ExecutionContextHandle, KernelContextRequest, StackHandle, StackRequest,
            ThreadResources, TlsHandle,
        },
        task_runtime,
    },
    sched::{CpuSet, SchedulePolicy},
    thread::{
        TaskError, ThreadExtension, ThreadHandle, ThreadSpec,
        execution::{PreparedThread, ThreadExecution, thread_entry},
    },
};

/// Default usable stack size for portable kernel service threads.
pub const DEFAULT_KERNEL_THREAD_STACK_SIZE: usize = 256 * 1024;

/// Configuration shared by kernel and user execution contexts.
#[derive(Debug)]
pub struct ThreadBuilder {
    name: String,
    stack_size: usize,
    stack_alignment: usize,
    guard_size: usize,
    policy: SchedulePolicy,
    affinity: Option<CpuSet>,
    os_extension: Option<ThreadExtension>,
}

impl ThreadBuilder {
    /// Starts a thread configuration with portable stack requirements.
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
    /// Sets the usable stack size in bytes.
    pub fn stack_size(mut self, size: usize) -> Self {
        self.stack_size = size;
        self
    }
    /// Sets stack alignment in bytes.
    pub fn stack_alignment(mut self, alignment: usize) -> Self {
        self.stack_alignment = alignment;
        self
    }
    /// Sets the inaccessible guard size in bytes.
    pub fn guard_size(mut self, size: usize) -> Self {
        self.guard_size = size;
        self
    }
    /// Sets the scheduling policy.
    pub fn policy(mut self, policy: SchedulePolicy) -> Self {
        self.policy = policy;
        self
    }
    /// Restricts initial and subsequent placement.
    pub fn affinity(mut self, affinity: CpuSet) -> Self {
        self.affinity = Some(affinity);
        self
    }
    /// Transfers an OS extension directly to the scheduler record.
    pub fn extension(mut self, extension: ThreadExtension) -> Self {
        self.os_extension = Some(extension);
        self
    }

    /// Creates a new, non-runnable kernel thread.
    pub fn prepare(
        self,
        entry: impl FnOnce() + Send + 'static,
    ) -> Result<PreparedThread, TaskError> {
        // SAFETY: the built-in allocator installs precisely the supplied trampoline
        // and transfers one complete runtime-owned resource bundle.
        unsafe {
            self.prepare_with(entry, |request, _trampoline| {
                allocate_thread_resources(runtime_task_system()?, request)
            })
        }
    }

    /// Creates and activates a kernel thread without an external publication transaction.
    pub fn spawn(self, entry: impl FnOnce() + Send + 'static) -> Result<ThreadHandle, TaskError> {
        self.prepare(entry)?.publish()
    }

    /// Creates a thread using an architecture-specific resource constructor.
    ///
    /// # Safety
    /// The constructor must install the supplied trampoline as its initial entry,
    /// return uniquely owned resources satisfying `ThreadResources::new`, and
    /// release every partial allocation on failure. It must not publish the context.
    pub unsafe fn prepare_with(
        mut self,
        entry: impl FnOnce() + Send + 'static,
        resources: impl FnOnce(
            StackRequest,
            unsafe extern "C" fn() -> !,
        ) -> Result<ThreadResources, TaskError>,
    ) -> Result<PreparedThread, TaskError> {
        validate_spec(&self)?;
        let system = runtime_task_system()?;
        let execution = crate::thread::allocation::try_arc(ThreadExecution::new(
            crate::thread::allocation::try_box(entry)?,
            core::mem::take(&mut self.name),
        ))?;
        let resources = resources(self.stack_request(), thread_entry)?;
        // SAFETY: the integration constructor transfers the owning resource bundle.
        let mut spec = unsafe { ThreadSpec::new(self.policy).with_resources(resources) };
        spec.execution = Some(execution);
        if let Some(extension) = self.os_extension.take() {
            spec = spec.with_extension(extension);
        }
        if let Some(affinity) = self.affinity.take() {
            spec = spec.with_affinity(affinity);
        }
        Ok(PreparedThread::new(system.create_thread(spec)?))
    }

    fn stack_request(&self) -> StackRequest {
        StackRequest {
            usable_size: self.stack_size,
            alignment: self.stack_alignment,
            guard_size: self.guard_size,
        }
    }
}

fn validate_spec(spec: &ThreadBuilder) -> Result<(), TaskError> {
    if spec.stack_size == 0 || spec.stack_alignment == 0 || !spec.stack_alignment.is_power_of_two()
    {
        Err(TaskError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

fn allocate_thread_resources(
    system: &crate::runtime::TaskSystem,
    request: StackRequest,
) -> Result<ThreadResources, TaskError> {
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
    let tls_result = task_runtime::allocate_kernel_tls();
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => {
            return Err(release_partial_thread_resources(
                system,
                stack,
                TlsHandle::NONE,
                TaskError::InvalidRuntimeHandle,
            ));
        }
        (RuntimeStatus::Success, handle) => {
            // SAFETY: successful TaskRuntime TLS allocation returns one
            // non-zero, uniquely owned handle live until deallocation.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => {
            return Err(release_partial_thread_resources(
                system,
                stack,
                TlsHandle::NONE,
                runtime_error(status),
            ));
        }
    };
    let context_result = task_runtime::create_kernel_context(KernelContextRequest {
        stack,
        entry: thread_entry,
        tls,
    });
    if context_result.status != RuntimeStatus::Success {
        return Err(release_partial_thread_resources(
            system,
            stack,
            tls,
            runtime_error(context_result.status),
        ));
    }
    if context_result.handle == 0 {
        return Err(release_partial_thread_resources(
            system,
            stack,
            tls,
            TaskError::InvalidRuntimeHandle,
        ));
    }
    Ok(unsafe {
        // SAFETY: all handles were just created by the active runtime and their
        // unique destruction rights move into the returned bundle.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context_result.handle),
            stack,
            tls,
            crate::runtime::resource::AddressSpaceToken::NONE,
        )
    })
}

fn release_partial_thread_resources(
    system: &crate::runtime::TaskSystem,
    stack: StackHandle,
    tls: TlsHandle,
    creation_error: TaskError,
) -> TaskError {
    let resources = unsafe {
        // SAFETY: this construction transaction uniquely owns both successful
        // allocations and has not created an execution context.
        ThreadResources::new(
            ExecutionContextHandle::NONE,
            stack,
            tls,
            crate::runtime::resource::AddressSpaceToken::NONE,
        )
    };
    system.release_unpublished_resources(resources);
    creation_error
}

const fn runtime_error(status: RuntimeStatus) -> TaskError {
    TaskError::RuntimeFailure(status as u32)
}
#[cfg(test)]
mod tests {
    use core::{
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::thread::{SwitchReason, ThreadExtensionOps, ThreadId};

    static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: test_extension_switch_in,
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
        let builder = ThreadBuilder::new(String::from("drop-test")).extension(extension);

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
        let spec = {
            // SAFETY: this test transfers the sole callback ownership.
            ThreadBuilder::new(String::from("invalid-test"))
                .stack_size(0)
                .extension(extension)
        };

        let result = validate_spec(&spec);
        drop(spec);

        assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    unsafe extern "Rust" fn test_extension_hook(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn test_extension_switch_in(
        _data: usize,
        _thread: ThreadId,
        _policy: SchedulePolicy,
        _charged_runtime_ns: u64,
    ) {
    }

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
