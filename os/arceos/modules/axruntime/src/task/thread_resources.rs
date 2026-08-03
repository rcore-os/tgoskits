use super::*;

pub(super) struct InitialContextState {
    pub(super) address_space: Option<TaskAddressSpace>,
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    pub(super) fp_state: Option<ax_hal::cpu::FpState>,
}

impl InitialContextState {
    pub(super) const fn kernel() -> Self {
        Self {
            address_space: None,
            #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
            fp_state: None,
        }
    }

    pub(super) fn user(address_space: TaskAddressSpace) -> Self {
        Self {
            address_space: Some(address_space),
            #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
            fp_state: None,
        }
    }
}

pub(super) fn create_idle_resources() -> ThreadResources {
    let guard_size = if cfg!(feature = "stack-guard-page") {
        PAGE_SIZE
    } else {
        0
    };
    let stack = allocate_runtime_stack(StackRequest {
        usable_size: runtime_task_stack_size(),
        alignment: 16,
        guard_size,
    })
    .unwrap_or_else(|status| panic!("failed to allocate idle stack: {status:?}"));
    let tls = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = if tls.status == RuntimeStatus::Success {
        assert_ne!(
            tls.handle, 0,
            "successful idle TLS allocation returned NONE"
        );
        // SAFETY: allocate_runtime_tls returned a fresh, non-zero allocation
        // whose ownership moves into the idle thread resources below.
        unsafe { TlsHandle::from_raw(tls.handle) }
    } else if tls.status == RuntimeStatus::Unsupported {
        TlsHandle::NONE
    } else {
        let _ = deallocate_runtime_stack(stack);
        panic!("failed to allocate idle TLS: {:?}", tls.status);
    };
    let context = create_runtime_context(KernelContextRequest {
        stack,
        entry: idle_context_entry,
        tls,
    });
    if context.status != RuntimeStatus::Success {
        let _ = deallocate_runtime_tls(tls);
        let _ = deallocate_runtime_stack(stack);
        panic!("failed to create idle context: {:?}", context.status);
    }
    unsafe {
        // SAFETY: the three fresh handles were created by this runtime and are
        // uniquely transferred into the idle record's resource bundle.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context.handle),
            stack,
            tls,
            ax_task::runtime::AddressSpaceToken::NONE,
        )
    }
}

pub(super) fn create_bootstrap_resources() -> Result<ThreadResources, TaskError> {
    let tls_result = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => return Err(TaskError::InvalidRuntimeHandle),
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the runtime returned a fresh, non-zero TLS allocation
            // whose unique ownership is transferred into bootstrap resources.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => return Err(runtime_status_error(status)),
    };
    let context = create_bootstrap_context();
    match assemble_bootstrap_resources(context, tls) {
        Ok(resources) => Ok(resources),
        Err(error) => {
            let _ = destroy_runtime_context(context);
            let _ = deallocate_runtime_tls(tls);
            Err(error)
        }
    }
}

pub(super) fn assemble_bootstrap_resources(
    context: ExecutionContextHandle,
    tls: TlsHandle,
) -> Result<ThreadResources, TaskError> {
    if context.is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    #[cfg(feature = "tls")]
    if tls.is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(unsafe {
        // SAFETY: the caller transfers the fresh bootstrap context and TLS
        // handles exactly once. Its architecture boot stack is externally
        // owned, so this resource bundle intentionally has no stack handle.
        ThreadResources::new(
            context,
            StackHandle::NONE,
            tls,
            ax_task::runtime::AddressSpaceToken::NONE,
        )
    })
}

pub(super) fn create_thread_resources(
    stack_size: usize,
    entry: ax_task::runtime::KernelEntry,
    context_state: InitialContextState,
) -> Result<ThreadResources, TaskError> {
    match create_thread_resources_with(
        &mut RuntimeThreadResourceBackend,
        stack_size,
        entry,
        context_state,
    ) {
        Ok(resources) => Ok(resources),
        Err(failure) => {
            let (error, unreleased) = failure.into_parts();
            if let Some(unreleased) = unreleased {
                let resources = unreleased.into_resources();
                let system = task_system().ok_or(TaskError::NotInitialized)?;
                system.release_unpublished_resources(resources);
            }
            Err(error)
        }
    }
}

pub(super) trait ThreadResourceBackend {
    fn allocate_stack(&mut self, request: StackRequest) -> Result<StackHandle, RuntimeStatus>;

    fn deallocate_stack(&mut self, stack: StackHandle) -> RuntimeStatus;

    fn allocate_tls(&mut self, request: TlsRequest) -> RuntimeHandleResult;

    fn deallocate_tls(&mut self, tls: TlsHandle) -> RuntimeStatus;

    fn create_kernel_context(&mut self, request: KernelContextRequest) -> RuntimeHandleResult;

    fn create_user_context(&mut self, request: UserContextRequest) -> RuntimeHandleResult;
}

#[derive(Debug)]
pub(super) struct ThreadResourceCreationFailure {
    error: TaskError,
    unreleased: Option<UnreleasedThreadResources>,
}

impl ThreadResourceCreationFailure {
    const fn new(error: TaskError) -> Self {
        Self {
            error,
            unreleased: None,
        }
    }

    const fn with_unreleased(error: TaskError, unreleased: UnreleasedThreadResources) -> Self {
        Self {
            error,
            unreleased: Some(unreleased),
        }
    }

    pub(super) fn into_parts(self) -> (TaskError, Option<UnreleasedThreadResources>) {
        (self.error, self.unreleased)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct UnreleasedThreadResources {
    pub(super) stack: StackHandle,
    pub(super) tls: TlsHandle,
}

impl UnreleasedThreadResources {
    fn into_resources(self) -> ThreadResources {
        unsafe {
            // SAFETY: the failed construction transaction still owns every
            // non-zero handle retained here and never created a context.
            ThreadResources::new(
                ExecutionContextHandle::NONE,
                self.stack,
                self.tls,
                ax_task::runtime::AddressSpaceToken::NONE,
            )
        }
    }
}

struct RuntimeThreadResourceBackend;

impl ThreadResourceBackend for RuntimeThreadResourceBackend {
    fn allocate_stack(&mut self, request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
        allocate_runtime_stack(request)
    }

    fn deallocate_stack(&mut self, stack: StackHandle) -> RuntimeStatus {
        deallocate_runtime_stack(stack)
    }

    fn allocate_tls(&mut self, request: TlsRequest) -> RuntimeHandleResult {
        allocate_runtime_tls(request)
    }

    fn deallocate_tls(&mut self, tls: TlsHandle) -> RuntimeStatus {
        deallocate_runtime_tls(tls)
    }

    fn create_kernel_context(&mut self, request: KernelContextRequest) -> RuntimeHandleResult {
        create_runtime_context(request)
    }

    fn create_user_context(&mut self, request: UserContextRequest) -> RuntimeHandleResult {
        create_user_runtime_context(request)
    }
}

pub(super) fn create_thread_resources_with(
    backend: &mut impl ThreadResourceBackend,
    stack_size: usize,
    entry: ax_task::runtime::KernelEntry,
    mut context_state: InitialContextState,
) -> Result<ThreadResources, ThreadResourceCreationFailure> {
    let guard_size = if cfg!(feature = "stack-guard-page") {
        PAGE_SIZE
    } else {
        0
    };
    let stack = backend
        .allocate_stack(StackRequest {
            usable_size: stack_size,
            alignment: 16,
            guard_size,
        })
        .map_err(|status| ThreadResourceCreationFailure::new(runtime_status_error(status)))?;
    if stack.is_none() {
        return Err(ThreadResourceCreationFailure::new(
            TaskError::InvalidRuntimeHandle,
        ));
    }
    let tls_result = backend.allocate_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => {
            return Err(rollback_thread_resource_creation(
                backend,
                stack,
                TlsHandle::NONE,
                TaskError::InvalidRuntimeHandle,
            ));
        }
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the runtime returned a fresh, non-zero TLS allocation
            // whose unique ownership moves into this thread's resources.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => {
            return Err(rollback_thread_resource_creation(
                backend,
                stack,
                TlsHandle::NONE,
                runtime_status_error(status),
            ));
        }
    };
    let address_space_handle = context_state
        .address_space
        .as_ref()
        .map_or(AddressSpaceHandle::NONE, TaskAddressSpace::handle);
    let context_result = if address_space_handle.is_none() {
        backend.create_kernel_context(KernelContextRequest { stack, entry, tls })
    } else {
        backend.create_user_context(UserContextRequest { stack, entry, tls })
    };
    if context_result.status != RuntimeStatus::Success {
        return Err(rollback_thread_resource_creation(
            backend,
            stack,
            tls,
            runtime_status_error(context_result.status),
        ));
    }
    if context_result.handle == 0 {
        return Err(rollback_thread_resource_creation(
            backend,
            stack,
            tls,
            TaskError::InvalidRuntimeHandle,
        ));
    }
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    if let Some(fp_state) = context_state.fp_state {
        context::install_initial_fp_state(context_result.handle, fp_state);
    }
    let address_space = context_state.address_space.as_mut().map_or(
        ax_task::runtime::AddressSpaceToken::NONE,
        TaskAddressSpace::take_token,
    );
    Ok(unsafe {
        // SAFETY: the active runtime created each live handle above and this is
        // the only owning bundle constructed from those scalar identities.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context_result.handle),
            stack,
            tls,
            address_space,
        )
    })
}

fn rollback_thread_resource_creation(
    backend: &mut impl ThreadResourceBackend,
    stack: StackHandle,
    tls: TlsHandle,
    error: TaskError,
) -> ThreadResourceCreationFailure {
    let retained_tls = if tls.is_none() || backend.deallocate_tls(tls) == RuntimeStatus::Success {
        TlsHandle::NONE
    } else {
        tls
    };
    let retained_stack = if backend.deallocate_stack(stack) == RuntimeStatus::Success {
        StackHandle::NONE
    } else {
        stack
    };
    if retained_tls.is_none() && retained_stack.is_none() {
        ThreadResourceCreationFailure::new(error)
    } else {
        ThreadResourceCreationFailure::with_unreleased(
            error,
            UnreleasedThreadResources {
                stack: retained_stack,
                tls: retained_tls,
            },
        )
    }
}
