use super::*;

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
    let tls = allocate_runtime_tls();
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
            ax_task::runtime::resource::AddressSpaceToken::NONE,
        )
    }
}

pub(super) fn create_bootstrap_resources() -> Result<ThreadResources, TaskError> {
    let tls_result = allocate_runtime_tls();
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
    #[cfg(kernel_tls)]
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
            ax_task::runtime::resource::AddressSpaceToken::NONE,
        )
    })
}

/// Owns partial runtime allocations until the complete bundle is transferred.
struct UnpublishedContext<'system> {
    system: &'system ax_task::runtime::TaskSystem,
    stack: StackHandle,
    tls: TlsHandle,
    context: ExecutionContextHandle,
}

impl UnpublishedContext<'_> {
    fn take_resources(
        &mut self,
        address_space: ax_task::runtime::resource::AddressSpaceToken,
    ) -> ThreadResources {
        // SAFETY: every non-zero handle was allocated by the installed runtime,
        // remains unpublished, and is removed from this sole transaction owner.
        unsafe {
            ThreadResources::new(
                core::mem::replace(&mut self.context, ExecutionContextHandle::NONE),
                core::mem::replace(&mut self.stack, StackHandle::NONE),
                core::mem::replace(&mut self.tls, TlsHandle::NONE),
                address_space,
            )
        }
    }
}

impl Drop for UnpublishedContext<'_> {
    fn drop(&mut self) {
        let resources = self.take_resources(ax_task::runtime::resource::AddressSpaceToken::NONE);
        self.system.release_unpublished_resources(resources);
    }
}

pub(super) fn create_user_resources(
    stack_request: StackRequest,
    entry: ax_task::runtime::resource::KernelEntry,
    mut options: UserContextOptions,
) -> Result<ThreadResources, TaskError> {
    // Establish the rollback owner before the first allocation. Release hooks
    // consume each resource once; they do not return retryable raw handles.
    let mut transaction = UnpublishedContext {
        system: task_system().ok_or(TaskError::NotInitialized)?,
        stack: StackHandle::NONE,
        tls: TlsHandle::NONE,
        context: ExecutionContextHandle::NONE,
    };
    transaction.stack = allocate_runtime_stack(stack_request).map_err(runtime_status_error)?;
    if transaction.stack.is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    let tls = allocate_runtime_tls();
    transaction.tls = match (tls.status, tls.handle) {
        (RuntimeStatus::Success, 0) => return Err(TaskError::InvalidRuntimeHandle),
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the successful provider call transfers one live TLS handle.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => return Err(runtime_status_error(status)),
    };
    let context = create_user_runtime_context(UserContextRequest {
        stack: transaction.stack,
        entry,
        tls: transaction.tls,
    });
    if context.status != RuntimeStatus::Success {
        return Err(runtime_status_error(context.status));
    }
    if context.handle == 0 {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    // SAFETY: successful creation transfers this context into the rollback owner
    // before architecture-specific initialization can fail or unwind.
    transaction.context = unsafe { ExecutionContextHandle::from_raw(context.handle) };
    #[cfg(feature = "fault-injection")]
    if super::creation_probe::record(super::creation_probe::CreationEvent::Fp) {
        return Err(TaskError::RuntimeFailure(RuntimeStatus::NoMemory as u32));
    }
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    if let Some(fp_state) = options.fp_state {
        context::install_initial_fp_state(context.handle, fp_state);
    }
    #[cfg(all(not(target_arch = "riscv64"), feature = "fp-simd", feature = "uspace"))]
    if options.inherit_current_fp {
        context::inherit_current_user_fp_state(context.handle);
    }
    let address_space = options.address_space.take_token();
    Ok(transaction.take_resources(address_space))
}
