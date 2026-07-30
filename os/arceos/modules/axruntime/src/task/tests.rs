use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn periodic_tick_without_task_work_does_not_request_reschedule() {
    assert!(!clock_event_requests_reschedule(false, false, 0, false));
}

#[test]
fn task_clock_event_outcomes_remain_sticky_reschedule_sources() {
    assert!(clock_event_requests_reschedule(true, false, 0, false));
    assert!(clock_event_requests_reschedule(false, true, 0, false));
    assert!(clock_event_requests_reschedule(false, false, 1, false));
    assert!(clock_event_requests_reschedule(false, false, 0, true));
}

static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_extension_thread_event,
    on_switch_out: ignore_extension_switch_out,
    on_exit: ignore_extension_thread_event,
    on_deadline_overrun: ignore_extension_thread_event,
    drop: count_extension_drop,
};

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
    CreateUserContext(usize),
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

    fn allocate_tls(&mut self, _request: TlsRequest) -> RuntimeHandleResult {
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

    fn create_user_context(&mut self, request: UserContextRequest) -> RuntimeHandleResult {
        self.events.push(ResourceEvent::CreateUserContext(
            request.address_space.into_raw(),
        ));
        match self.failure {
            InjectedResourceFailure::Context | InjectedResourceFailure::TlsRollback => {
                RuntimeHandleResult::failure(RuntimeStatus::NoMemory)
            }
            InjectedResourceFailure::MissingContextHandle => RuntimeHandleResult::success(0),
            _ => RuntimeHandleResult::success(0x3000),
        }
    }
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
#[test]
fn scheduler_ipi_doorbell_coalesces_and_consumes_publication() {
    let doorbell = SchedulerIpiDoorbell::new();

    assert!(!doorbell.consume());
    assert!(doorbell.publish());
    assert!(!doorbell.publish());
    assert!(doorbell.consume());
    assert!(!doorbell.consume());
}

#[test]
fn scheduler_ipi_notification_follows_successful_publication() {
    let events = core::cell::RefCell::new(alloc::vec::Vec::new());

    assert_eq!(
        publish_then_notify_scheduler_ipi(
            || {
                events.borrow_mut().push("publish");
                RuntimeStatus::Success
            },
            || events.borrow_mut().push("notify"),
        ),
        RuntimeStatus::Success
    );
    assert_eq!(*events.borrow(), ["publish", "notify"]);
}

#[test]
fn failed_scheduler_ipi_publication_suppresses_notification() {
    let notified = AtomicBool::new(false);

    assert_eq!(
        publish_then_notify_scheduler_ipi(
            || RuntimeStatus::NotInitialized,
            || notified.store(true, Ordering::Release),
        ),
        RuntimeStatus::NotInitialized
    );
    assert!(!notified.load(Ordering::Acquire));
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
#[test]
fn coalesced_scheduler_ipi_publication_suppresses_a_duplicate_notification() {
    let doorbell = SchedulerIpiDoorbell::new();
    let notifications = AtomicUsize::new(0);
    let publish = || {
        if doorbell.publish() {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::Busy
        }
    };

    assert_eq!(
        publish_then_notify_scheduler_ipi(publish, || {
            notifications.fetch_add(1, Ordering::Relaxed);
        }),
        RuntimeStatus::Success
    );
    assert_eq!(
        publish_then_notify_scheduler_ipi(publish, || {
            notifications.fetch_add(1, Ordering::Relaxed);
        }),
        RuntimeStatus::Busy
    );
    assert_eq!(notifications.load(Ordering::Relaxed), 1);
    assert!(doorbell.consume());
}

#[test]
fn missing_outer_runtime_extension_is_not_an_error() {
    assert_eq!(
        classify_runtime_extension(None, 0),
        Ok(RuntimeExtensionKind::Missing)
    );
}

#[test]
fn foreign_outer_runtime_extension_remains_an_error() {
    assert_eq!(
        classify_runtime_extension(Some(&TEST_EXTENSION_OPS), usize::MAX),
        Err(TaskError::InvalidConfiguration)
    );
}

#[test]
fn matching_runtime_ops_reject_malformed_extension_data() {
    assert_eq!(
        classify_runtime_extension(Some(&RUNTIME_THREAD_EXTENSION_OPS), 0),
        Err(TaskError::InvalidRuntimeHandle)
    );
    assert_eq!(
        classify_runtime_extension(Some(&RUNTIME_THREAD_EXTENSION_OPS), 1),
        Err(TaskError::InvalidRuntimeHandle)
    );

    let data = RuntimeThreadData {
        entry: SpinNoIrq::new(None),
        exit_code: AtomicI32::new(0),
        exit_completed: AtomicBool::new(false),
        join_wait: WaitQueue::new(),
        os_extension: None,
        _name: String::new(),
    };
    assert_eq!(
        classify_runtime_extension(
            Some(&RUNTIME_THREAD_EXTENSION_OPS),
            core::ptr::from_ref(&data).expose_provenance(),
        ),
        Ok(RuntimeExtensionKind::Runtime)
    );
}

#[test]
fn runtime_outer_extension_forwards_os_scheduler_tick_work() {
    let callbacks = AtomicUsize::new(0);
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let callback_data = (&callbacks as *const AtomicUsize).expose_provenance();
    let os_extension = unsafe {
        ThreadExtension::new(callback_data, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, count_scheduler_tick_work)
    };
    let data = Box::into_raw(Box::new(RuntimeThreadData::new(
        Box::new(|| {}),
        String::from("tick-forwarding"),
        Some(os_extension),
    )))
    .expose_provenance();
    let outer = unsafe { runtime_thread_extension(data) };

    assert!(
        outer.scheduler_tick_work_gate().is_some(),
        "the scheduler-owned outer extension must retain OS tick interest"
    );
    assert!(unsafe { outer.forward_scheduler_tick_work(ThreadId::from_parts(1, 1)) });
    assert_eq!(callbacks.load(Ordering::Acquire), 1);
    drop(outer);
}

#[test]
fn invalid_spawn_releases_transferred_extension() {
    let extension_drops = AtomicUsize::new(0);
    // SAFETY: the call fails synchronously and drops the extension before
    // this stack-owned counter leaves scope.
    let extension = unsafe {
        ThreadExtension::new(
            (&extension_drops as *const AtomicUsize).expose_provenance(),
            &TEST_EXTENSION_OPS,
        )
    };

    // SAFETY: this call transfers the test extension's unique logical ownership.
    let result = unsafe {
        spawn_raw_with_extension_and_affinity(
            || {},
            String::from("invalid-stack"),
            0,
            Some(extension),
            None,
        )
    };

    assert!(matches!(result, Err(TaskError::InvalidConfiguration)));
    assert_eq!(extension_drops.load(Ordering::Acquire), 1);
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
            4096,
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
            4096,
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
    let address_space = TaskAddressSpace::from_page_table_root(0x4000).unwrap();

    let result = create_thread_resources_with(
        &mut backend,
        4096,
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
            ResourceEvent::CreateUserContext(0x4000),
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

#[test]
fn entry_extension_lookup_does_not_pin_exited_thread() {
    let extension_drops = AtomicUsize::new(0);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let extension_data = (&extension_drops as *const AtomicUsize).expose_provenance();
    // SAFETY: this test reaps the thread and runs the matching drop callback
    // before the stack-owned counter leaves scope.
    let extension = unsafe { ThreadExtension::new(extension_data, &TEST_EXTENSION_OPS) };
    let spec = ThreadSpec::new(SchedulePolicy::default()).with_extension(extension);
    let handle = system.create_thread(spec).unwrap();
    let lease = system
        .thread_extension_lease(handle.clone())
        .unwrap()
        .unwrap();

    assert_eq!(
        extension_data_after_releasing_lease(lease, &TEST_EXTENSION_OPS).unwrap(),
        extension_data
    );
    system.mark_exited(handle.id()).unwrap();
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress(),
        "the exit callback must finish before the test isolates extension-lease ownership"
    );
    system.reap_thread_handle(handle).unwrap();
    assert_eq!(extension_drops.load(Ordering::Acquire), 1);
}

#[test]
fn user_context_rejects_a_missing_address_space() {
    let result = create_user_runtime_context(UserContextRequest {
        stack: StackHandle::NONE,
        entry: unreachable_test_entry,
        tls: TlsHandle::NONE,
        address_space: AddressSpaceHandle::NONE,
    });

    assert_eq!(result.status, RuntimeStatus::InvalidHandle);
    assert_eq!(result.handle, 0);
}

#[cfg(feature = "tls")]
#[test]
fn bootstrap_thread_rejects_a_missing_tls_resource() {
    // SAFETY: this inert non-zero identity is never dereferenced because
    // validation rejects the missing TLS resource first.
    let context = unsafe { ExecutionContextHandle::from_raw(1) };
    let result = assemble_bootstrap_resources(context, TlsHandle::NONE);

    assert!(matches!(result, Err(TaskError::InvalidRuntimeHandle)));
}

unsafe extern "Rust" fn ignore_extension_thread_event(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: SwitchReason,
) {
}

unsafe extern "C" fn unreachable_test_entry() -> ! {
    panic!("invalid user context must not invoke its entry")
}

unsafe extern "Rust" fn count_extension_drop(data: usize) {
    // SAFETY: each test keeps its stack-owned counter live until it
    // synchronously observes the extension's matching drop callback.
    let drops = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
    drops.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn count_scheduler_tick_work(data: usize, _thread: ThreadId) {
    let callbacks = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
    callbacks.fetch_add(1, Ordering::Release);
}
