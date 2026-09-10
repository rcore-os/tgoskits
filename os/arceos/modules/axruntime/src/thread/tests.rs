use alloc::vec::Vec;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InjectedResourceFailure {
    Stack,
    MissingStackHandle,
    Tls,
    MissingTlsHandle,
    Context,
    MissingContextHandle,
    StackRollback,
    TlsRollback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceEvent {
    AllocateStack,
    AllocateTls,
    CreateKernelContext,
    CreateUserContext,
    DeallocateTls,
    DeallocateStack,
}

struct InjectedResourceBackend {
    failure: InjectedResourceFailure,
    events: Vec<ResourceEvent>,
}

impl InjectedResourceBackend {
    fn new(failure: InjectedResourceFailure) -> Self {
        Self {
            failure,
            events: Vec::new(),
        }
    }
}

impl ThreadResourceBackend for InjectedResourceBackend {
    fn allocate_stack(&mut self, _request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
        self.events.push(ResourceEvent::AllocateStack);
        match self.failure {
            InjectedResourceFailure::Stack => Err(RuntimeStatus::NoMemory),
            InjectedResourceFailure::MissingStackHandle => Ok(StackHandle::NONE),
            _ => {
                // SAFETY: the injected backend owns this inert identity and
                // intercepts every matching deallocation in the same test.
                Ok(unsafe { StackHandle::from_raw(0x1000) })
            }
        }
    }

    fn deallocate_stack(&mut self, _stack: StackHandle) -> RuntimeStatus {
        self.events.push(ResourceEvent::DeallocateStack);
        if self.failure == InjectedResourceFailure::StackRollback {
            RuntimeStatus::Busy
        } else {
            RuntimeStatus::Success
        }
    }

    fn allocate_kernel_tls(&mut self) -> RuntimeHandleResult {
        self.events.push(ResourceEvent::AllocateTls);
        match self.failure {
            InjectedResourceFailure::Tls | InjectedResourceFailure::StackRollback => {
                RuntimeHandleResult::failure(RuntimeStatus::NoMemory)
            }
            InjectedResourceFailure::MissingTlsHandle => {
                RuntimeHandleResult::success(TlsHandle::NONE.into_raw())
            }
            _ => RuntimeHandleResult::success(0x2000),
        }
    }

    fn deallocate_tls(&mut self, _tls: TlsHandle) -> RuntimeStatus {
        self.events.push(ResourceEvent::DeallocateTls);
        if self.failure == InjectedResourceFailure::TlsRollback {
            RuntimeStatus::Busy
        } else {
            RuntimeStatus::Success
        }
    }

    fn create_kernel_context(&mut self, _request: KernelContextRequest) -> RuntimeHandleResult {
        self.events.push(ResourceEvent::CreateKernelContext);
        match self.failure {
            InjectedResourceFailure::Context | InjectedResourceFailure::TlsRollback => {
                RuntimeHandleResult::failure(RuntimeStatus::NoMemory)
            }
            InjectedResourceFailure::MissingContextHandle => RuntimeHandleResult::success(0),
            _ => RuntimeHandleResult::success(0x3000),
        }
    }

    fn create_user_context(&mut self, _request: UserContextRequest) -> RuntimeHandleResult {
        self.events.push(ResourceEvent::CreateUserContext);
        match self.failure {
            InjectedResourceFailure::Context | InjectedResourceFailure::TlsRollback => {
                RuntimeHandleResult::failure(RuntimeStatus::NoMemory)
            }
            InjectedResourceFailure::MissingContextHandle => RuntimeHandleResult::success(0),
            _ => RuntimeHandleResult::success(0x3000),
        }
    }
}

#[test]
fn thread_resource_creation_rolls_back_every_failed_stage() {
    let cases: &[(InjectedResourceFailure, &[ResourceEvent])] = &[
        (
            InjectedResourceFailure::Stack,
            &[ResourceEvent::AllocateStack],
        ),
        (
            InjectedResourceFailure::MissingStackHandle,
            &[ResourceEvent::AllocateStack],
        ),
        (
            InjectedResourceFailure::Tls,
            &[
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
        (
            InjectedResourceFailure::MissingTlsHandle,
            &[
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
        (
            InjectedResourceFailure::Context,
            &[
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::CreateKernelContext,
                ResourceEvent::DeallocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
        (
            InjectedResourceFailure::MissingContextHandle,
            &[
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::CreateKernelContext,
                ResourceEvent::DeallocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
    ];

    for &(injected, expected_events) in cases {
        let mut backend = InjectedResourceBackend::new(injected);
        let result = create_thread_resources_with(
            &mut backend,
            StackRequest {
                usable_size: 4096,
                alignment: 16,
                guard_size: 0,
            },
            unreachable_test_entry,
            InitialContextState::kernel(),
        );

        match (injected, result) {
            (
                InjectedResourceFailure::MissingTlsHandle
                | InjectedResourceFailure::MissingStackHandle
                | InjectedResourceFailure::MissingContextHandle,
                Err(failure),
            ) => {
                let (error, unreleased) = failure.into_parts();
                assert_eq!(error, TaskError::InvalidRuntimeHandle);
                assert_eq!(unreleased, None);
            }
            (_, Err(failure)) => {
                let (error, unreleased) = failure.into_parts();
                assert_eq!(
                    error,
                    TaskError::RuntimeFailure(RuntimeStatus::NoMemory as u32)
                );
                assert_eq!(unreleased, None);
            }
            (_, Ok(_)) => panic!("injected resource failure unexpectedly succeeded"),
        }
        assert_eq!(backend.events, expected_events);
    }
}

#[test]
fn failed_resource_rollback_returns_every_live_handle() {
    let cases = [
        (
            InjectedResourceFailure::StackRollback,
            UnreleasedThreadResources {
                stack: unsafe {
                    // SAFETY: the injected backend treats this as an inert
                    // identity and deliberately rejects its first release.
                    StackHandle::from_raw(0x1000)
                },
                tls: TlsHandle::NONE,
            },
            alloc::vec![
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
        (
            InjectedResourceFailure::TlsRollback,
            UnreleasedThreadResources {
                stack: StackHandle::NONE,
                tls: unsafe {
                    // SAFETY: the injected backend treats this as an inert
                    // identity and deliberately rejects its first release.
                    TlsHandle::from_raw(0x2000)
                },
            },
            alloc::vec![
                ResourceEvent::AllocateStack,
                ResourceEvent::AllocateTls,
                ResourceEvent::CreateKernelContext,
                ResourceEvent::DeallocateTls,
                ResourceEvent::DeallocateStack,
            ],
        ),
    ];

    for (injected, expected_unreleased, expected_events) in cases {
        let mut backend = InjectedResourceBackend::new(injected);
        let failure = create_thread_resources_with(
            &mut backend,
            StackRequest {
                usable_size: 4096,
                alignment: 16,
                guard_size: 0,
            },
            unreachable_test_entry,
            InitialContextState::kernel(),
        )
        .unwrap_err();
        let (error, unreleased) = failure.into_parts();

        assert_eq!(
            error,
            TaskError::RuntimeFailure(RuntimeStatus::NoMemory as u32)
        );
        assert_eq!(unreleased, Some(expected_unreleased));
        assert_eq!(backend.events, expected_events);
    }
}

#[test]
fn failed_user_context_creation_preserves_address_space_identity_during_rollback() {
    let mut backend = InjectedResourceBackend::new(InjectedResourceFailure::Context);
    let address_space = TaskAddressSpace::new(ax_memory_addr::PhysAddr::from(0x4000), ()).unwrap();

    let result = create_thread_resources_with(
        &mut backend,
        StackRequest {
            usable_size: 4096,
            alignment: 16,
            guard_size: 0,
        },
        unreachable_test_entry,
        InitialContextState::user(address_space),
    );

    let (error, unreleased) = result.unwrap_err().into_parts();
    assert_eq!(
        error,
        TaskError::RuntimeFailure(RuntimeStatus::NoMemory as u32)
    );
    assert_eq!(unreleased, None);
    assert_eq!(
        backend.events,
        [
            ResourceEvent::AllocateStack,
            ResourceEvent::AllocateTls,
            ResourceEvent::CreateUserContext,
            ResourceEvent::DeallocateTls,
            ResourceEvent::DeallocateStack,
        ]
    );
}

#[test]
fn secondary_bootstrap_retires_before_entering_idle_loop() {
    let bootstrap = ThreadId::from_parts(1, 1);
    let idle = ThreadId::from_parts(2, 1);

    assert_eq!(
        idle_entry_action(Some(bootstrap), Some(idle)).unwrap(),
        IdleEntryAction::RetireBootstrap,
    );
    assert_eq!(
        idle_entry_action(Some(idle), Some(idle)).unwrap(),
        IdleEntryAction::RunIdle,
    );
}

#[cfg(kernel_tls)]
#[test]
fn bootstrap_thread_rejects_a_missing_tls_resource() {
    // SAFETY: this inert non-zero identity is never dereferenced because
    // validation rejects the missing TLS resource first.
    let context = unsafe { ExecutionContextHandle::from_raw(1) };
    let result = assemble_bootstrap_resources(context, TlsHandle::NONE);

    assert!(matches!(result, Err(TaskError::InvalidRuntimeHandle)));
}

unsafe extern "C" fn unreachable_test_entry() -> ! {
    panic!("invalid context must not enter")
}
