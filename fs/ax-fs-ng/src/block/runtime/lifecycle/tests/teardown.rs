use super::{resource_rollback::DropTrackedQueue, *};

struct BlockingGate {
    state: StdMutex<BlockingGateState>,
    changed: std::sync::Condvar,
}

struct BlockingGateState {
    entered: bool,
    released: bool,
}

impl BlockingGate {
    fn new() -> Self {
        Self {
            state: StdMutex::new(BlockingGateState {
                entered: false,
                released: false,
            }),
            changed: std::sync::Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.entered = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn wait_until_entered(&self, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| !state.entered)
            .unwrap();
        state.entered
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.released = true;
        self.changed.notify_all();
    }
}

struct BlockingIrqRegistration {
    disable_gate: Arc<BlockingGate>,
}

impl BlockIrqRegistration for BlockingIrqRegistration {
    fn enable(&self) -> BlockResult {
        Ok(())
    }

    fn disable_and_synchronize(&self) -> BlockResult {
        self.disable_gate.enter_and_wait();
        Ok(())
    }
}

struct LateRollbackController {
    active_queue: Option<Box<dyn HardwareQueue>>,
    late_queue: Option<Box<dyn HardwareQueue>>,
    update_gate: Arc<BlockingGate>,
}

impl DriverGeneric for LateRollbackController {
    fn name(&self) -> &str {
        "late-rollback-controller"
    }
}

impl BlockController for LateRollbackController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        2
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![self.active_queue.take().ok_or(BlkError::Io)?],
                Vec::new(),
            )),
            ControllerEvent::Irq(_) => {
                self.update_gate.enter_and_wait();
                Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    vec![self.late_queue.take().ok_or(BlkError::Io)?],
                    vec![IrqEndpoint::new(0, 1 << 1, Box::new(SpuriousHandler))],
                ))
            }
            ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct QueueTeardownController {
    start_queue: Option<Box<dyn HardwareQueue>>,
    shutdown_queue: Option<Box<dyn HardwareQueue>>,
}

impl DriverGeneric for QueueTeardownController {
    fn name(&self) -> &str {
        "queue-teardown-controller"
    }
}

impl BlockController for QueueTeardownController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![self.start_queue.take().ok_or(BlkError::Io)?],
                Vec::new(),
            )),
            ControllerEvent::Shutdown => Ok(self.shutdown_queue.take().map_or_else(
                || ControllerUpdate::state(ControllerState::Shutdown),
                |queue| {
                    ControllerUpdate::with_resources(
                        ControllerState::Shutdown,
                        vec![queue],
                        Vec::new(),
                    )
                },
            )),
            ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

fn wait_for_group_teardown(group: &BlockGroupHandle) {
    while group.teardown_state.load(Ordering::Acquire) != GROUP_STOPPED {
        group
            .teardown_waiters
            .wait_while(|| group.teardown_state.load(Ordering::Acquire) != GROUP_STOPPED)
            .unwrap();
    }
}

#[test]
fn group_teardown_wakes_every_concurrent_waiter() {
    crate::os::task::install_test_runtime_ops();
    let group = Arc::new(BlockGroupHandle {
        name: String::from("concurrent-teardown"),
        controller: IrqMutex::new(None),
        registrations: IrqMutex::new(Vec::new()),
        members: Vec::new(),
        teardown_state: AtomicU8::new(GROUP_STOPPING),
        teardown_waiters: TaskWaiters::new(),
    });
    let (completed_tx, completed_rx) = mpsc::channel();
    let waiters = (0..2)
        .map(|_| {
            let group = Arc::clone(&group);
            let completed_tx = completed_tx.clone();
            thread::spawn(move || {
                completed_tx.send(group.shutdown_result()).unwrap();
            })
        })
        .collect::<Vec<_>>();
    drop(completed_tx);

    while group.teardown_waiters.len() != 2 {
        thread::yield_now();
    }
    assert_eq!(group.finish_shutdown(Ok(1)), Ok(1));
    let first = completed_rx.recv_timeout(Duration::from_millis(100));
    let second = completed_rx.recv_timeout(Duration::from_millis(100));
    for waiter in waiters {
        waiter.join().unwrap();
    }

    assert_eq!(first, Ok(Ok(0)));
    assert_eq!(second, Ok(Ok(0)), "all teardown waiters must be woken");
}

#[test]
fn last_device_handle_drop_owns_teardown_despite_internal_references() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "drop-owner",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(20)),
        }],
        Box::new(LifecycleController {
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
            repeat_device_info_on_quiesce: false,
        }),
    ))
    .unwrap();
    let temporary_internal_owner = Arc::clone(&handle.inner);

    drop(handle);

    assert!(
        log.lock().unwrap().contains(&"controller_shutdown"),
        "the last user-facing handle must own teardown"
    );
    drop(temporary_internal_owner);
}

#[test]
fn active_queue_shutdown_failure_is_reported_and_quarantined() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "active-queue-shutdown-failure",
        [],
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::shutdown_failure(
                0,
                "failed_active_queue_drop",
                Arc::clone(&log),
                BlkError::TimedOut,
            ))),
            shutdown_queue: None,
        }),
    ))
    .unwrap();
    let controller = Arc::downgrade(&handle.inner.controller);

    assert_eq!(handle.inner.shutdown_result(), Err(BlockError::TimedOut));
    assert_eq!(handle.inner.shutdown_result(), Err(BlockError::TimedOut));
    assert!(
        !log.lock().unwrap().contains(&"failed_active_queue_drop"),
        "a queue that may still be DMA-visible must remain quarantined"
    );
    drop(handle);
    assert!(!log.lock().unwrap().contains(&"failed_active_queue_drop"));
    assert!(
        controller.upgrade().is_none(),
        "terminal queue failure must not leak the quiesced controller"
    );
}

#[test]
fn detached_queue_shutdown_failure_is_reported_and_quarantined() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "detached-queue-shutdown-failure",
        [],
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::startable(
                0,
                "active_queue_drop",
                Arc::clone(&log),
            ))),
            shutdown_queue: None,
        }),
    ))
    .unwrap();
    handle.inner.detached_queues.lock().extend([
        Box::new(DropTrackedQueue::shutdown_failure(
            1,
            "failed_detached_queue_drop",
            Arc::clone(&log),
            BlkError::TimedOut,
        )) as Box<dyn HardwareQueue>,
        Box::new(DropTrackedQueue::shutdown_failure(
            2,
            "second_failed_detached_queue_drop",
            Arc::clone(&log),
            BlkError::InvalidRequest,
        )),
        Box::new(DropTrackedQueue::startable(
            3,
            "successful_detached_queue_drop",
            Arc::clone(&log),
        )),
    ]);

    let first_result = handle.inner.shutdown_result();
    let second_result = handle.inner.shutdown_result();
    let detached_is_empty = handle.inner.detached_queues.lock().is_empty();
    let (active_dropped, successful_dropped, failed_dropped, second_failed_dropped) = {
        let log = log.lock().unwrap();
        (
            log.contains(&"active_queue_drop"),
            log.contains(&"successful_detached_queue_drop"),
            log.contains(&"failed_detached_queue_drop"),
            log.contains(&"second_failed_detached_queue_drop"),
        )
    };
    drop(handle);
    assert_eq!(first_result, Err(BlockError::TimedOut));
    assert_eq!(second_result, Err(BlockError::TimedOut));
    assert!(detached_is_empty);
    assert!(active_dropped);
    assert!(successful_dropped);
    assert!(
        !failed_dropped,
        "a failed detached queue must remain owned by the quarantine"
    );
    assert!(!second_failed_dropped);
    assert!(!log.lock().unwrap().contains(&"failed_detached_queue_drop"));
    assert!(
        !log.lock()
            .unwrap()
            .contains(&"second_failed_detached_queue_drop")
    );
}

#[test]
fn teardown_shutdowns_queue_rolled_back_while_shutdown_is_queued() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(usize::MAX, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let disable_gate = Arc::new(BlockingGate::new());
    let update_gate = Arc::new(BlockingGate::new());
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "late-rollback-during-shutdown",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(26)),
        }],
        Box::new(LateRollbackController {
            active_queue: Some(Box::new(DropTrackedQueue::startable(
                0,
                "active_queue_drop",
                Arc::clone(&log),
            ))),
            late_queue: Some(Box::new(DropTrackedQueue::shutdown_failure(
                1,
                "late_queue_drop",
                Arc::clone(&log),
                BlkError::TimedOut,
            ))),
            update_gate: Arc::clone(&update_gate),
        }),
    ))
    .unwrap();
    let (_, controller_token) = handle.inner.controller.prepare_irq_target(0);
    handle
        .inner
        .irq_registrations
        .lock()
        .push(InstalledIrqRegistration {
            registration: Box::new(BlockingIrqRegistration {
                disable_gate: Arc::clone(&disable_gate),
            }),
            hctx_tokens: Vec::new(),
            controller_token,
        });

    let device = Arc::clone(&handle.inner);
    let shutdown = thread::spawn(move || device.shutdown_result());
    if !disable_gate.wait_until_entered(Duration::from_secs(1)) {
        disable_gate.release();
        update_gate.release();
        panic!("shutdown did not begin IRQ synchronization");
    }
    handle
        .inner
        .controller
        .post(ControllerEvent::Irq(ControlEvent::new(0, 1)));
    if !update_gate.wait_until_entered(Duration::from_secs(1)) {
        disable_gate.release();
        update_gate.release();
        panic!("controller did not begin the late IRQ update");
    }
    disable_gate.release();
    let queued_deadline = Instant::now() + Duration::from_secs(1);
    while handle.inner.controller.commands.queued_len() == 0 && Instant::now() < queued_deadline {
        thread::yield_now();
    }
    let shutdown_was_queued = handle.inner.controller.commands.queued_len() != 0;
    update_gate.release();

    let result = shutdown.join().unwrap();
    let detached_is_empty = handle.inner.detached_queues.lock().is_empty();
    let registrations_are_empty = handle.inner.irq_registrations.lock().is_empty();
    let (active_dropped, late_irq_registered, late_queue_dropped) = {
        let log = log.lock().unwrap();
        (
            log.contains(&"active_queue_drop"),
            log.contains(&"irq_register_disabled"),
            log.contains(&"late_queue_drop"),
        )
    };
    drop(handle);
    assert!(
        shutdown_was_queued,
        "shutdown was not queued behind the late IRQ update"
    );
    assert_eq!(result, Err(BlockError::TimedOut));
    assert!(detached_is_empty);
    assert!(registrations_are_empty);
    assert!(active_dropped);
    assert!(!late_irq_registered);
    assert!(!late_queue_dropped);
    assert!(!log.lock().unwrap().contains(&"late_queue_drop"));
}

#[test]
fn runtime_teardown_continues_after_terminal_device_error() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let runtime = BlockRuntime::from_rdif_sources(
        [
            RdifBlockDevice::new_with_irqs(
                "failed-runtime-device",
                [],
                Box::new(QueueTeardownController {
                    start_queue: Some(Box::new(DropTrackedQueue::shutdown_failure(
                        0,
                        "failed_runtime_queue_drop",
                        Arc::clone(&log),
                        BlkError::TimedOut,
                    ))),
                    shutdown_queue: None,
                }),
            ),
            RdifBlockDevice::new_with_irqs(
                "healthy-runtime-device",
                [],
                Box::new(QueueTeardownController {
                    start_queue: Some(Box::new(DropTrackedQueue::startable(
                        0,
                        "healthy_runtime_queue_drop",
                        Arc::clone(&log),
                    ))),
                    shutdown_queue: None,
                }),
            ),
        ],
        Vec::new(),
    );

    let result = runtime.release_irqs_for_passthrough();
    let (failed_dropped, healthy_dropped) = {
        let log = log.lock().unwrap();
        (
            log.contains(&"failed_runtime_queue_drop"),
            log.contains(&"healthy_runtime_queue_drop"),
        )
    };
    drop(runtime);

    assert_eq!(result, Err(BlockError::TimedOut));
    assert!(!failed_dropped);
    assert!(
        healthy_dropped,
        "one terminal device error must not skip later devices"
    );
}

#[test]
fn group_queue_shutdown_failure_is_reported_and_quarantined() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(usize::MAX, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let failed_member = BlockGroupMember::new(
        0,
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::shutdown_failure(
                0,
                "failed_group_queue_drop",
                Arc::clone(&log),
                BlkError::TimedOut,
            ))),
            shutdown_queue: None,
        }),
    );
    let successful_member = BlockGroupMember::new(
        1,
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::startable(
                0,
                "successful_group_queue_drop",
                Arc::clone(&log),
            ))),
            shutdown_queue: None,
        }),
    );
    let trailing_member = BlockGroupMember::new(
        0,
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::startable(
                0,
                "trailing_group_queue_drop",
                Arc::clone(&log),
            ))),
            shutdown_queue: None,
        }),
    );
    let runtime = BlockRuntime::from_rdif_sources(
        [RdifBlockDevice::new_with_irqs(
            "post-group-healthy-device",
            [],
            Box::new(QueueTeardownController {
                start_queue: Some(Box::new(DropTrackedQueue::startable(
                    0,
                    "post_group_device_queue_drop",
                    Arc::clone(&log),
                ))),
                shutdown_queue: None,
            }),
        )],
        [
            RdifBlockGroup::new_with_irqs(
                "group-queue-shutdown-failure",
                [BlockIrqSource {
                    source_id: 0,
                    irq: IrqId::new(IrqDomainId(1), HwIrq(25)),
                }],
                Box::new(TestControllerGroup {
                    members: Some(vec![failed_member, successful_member]),
                    log: Arc::clone(&log),
                }),
            ),
            RdifBlockGroup::new_with_irqs(
                "trailing-healthy-group",
                [BlockIrqSource {
                    source_id: 0,
                    irq: IrqId::new(IrqDomainId(1), HwIrq(27)),
                }],
                Box::new(TestControllerGroup {
                    members: Some(vec![trailing_member]),
                    log: Arc::clone(&log),
                }),
            ),
        ],
    );
    let successful_member = Arc::downgrade(&runtime.groups[0].members[1].inner);
    let trailing_member = Arc::downgrade(&runtime.groups[1].members[0].inner);

    let first_result = runtime.release_irqs_for_passthrough();
    let first_group_stopped =
        runtime.groups[0].teardown_state.load(Ordering::Acquire) == GROUP_STOPPED;
    let second_result = runtime.release_irqs_for_passthrough();
    let (successful_dropped, trailing_dropped, device_dropped, failed_dropped) = {
        let log = log.lock().unwrap();
        (
            log.contains(&"successful_group_queue_drop"),
            log.contains(&"trailing_group_queue_drop"),
            log.contains(&"post_group_device_queue_drop"),
            log.contains(&"failed_group_queue_drop"),
        )
    };
    drop(runtime);
    assert_eq!(first_result, Err(BlockError::TimedOut));
    assert!(first_group_stopped);
    assert_eq!(second_result, Err(BlockError::TimedOut));
    assert!(successful_dropped);
    assert!(
        trailing_dropped,
        "one terminal group error must not skip later groups"
    );
    assert!(
        device_dropped,
        "a terminal group error must not skip standalone devices"
    );
    assert!(
        !failed_dropped,
        "one failed member must not prevent the group from quarantining its queue"
    );
    assert!(!log.lock().unwrap().contains(&"failed_group_queue_drop"));
    assert!(
        successful_member.upgrade().is_none(),
        "one failed queue must not leak successful group members"
    );
    assert!(trailing_member.upgrade().is_none());
}

#[test]
fn group_irq_failure_does_not_bypass_shared_owner() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(usize::MAX, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let member = GroupMemberController {
        name: "group-irq-failure-member",
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
        terminal_on_rearm: false,
        rearm_count: 0,
    };
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "group-irq-failure",
            [BlockIrqSource {
                source_id: 0,
                irq: IrqId::new(IrqDomainId(1), HwIrq(28)),
            }],
            Box::new(TestControllerGroup {
                members: Some(vec![BlockGroupMember::new(0, Box::new(member))]),
                log: Arc::clone(&log),
            }),
        )],
    );

    TEST_IRQ_FAIL_SYNCHRONIZE.store(true, Ordering::Release);
    let result = runtime.release_irqs_for_passthrough();
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    let (quiesce_count, member_shutdown) = {
        let log = log.lock().unwrap();
        (
            log.iter()
                .filter(|event| **event == "member_quiesce")
                .count(),
            log.contains(&"member_shutdown"),
        )
    };
    drop(runtime);

    assert_eq!(result, Err(BlockError::Io));
    assert_eq!(
        quiesce_count, 1,
        "runtime teardown must not bypass a member's shared IRQ owner"
    );
    assert!(!member_shutdown);
}

#[test]
fn rejected_shutdown_update_keeps_emitted_queue_quarantined() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "shutdown-update-queue",
        [],
        Box::new(QueueTeardownController {
            start_queue: Some(Box::new(DropTrackedQueue::startable(
                0,
                "active_queue_drop",
                Arc::clone(&log),
            ))),
            shutdown_queue: Some(Box::new(DropTrackedQueue::startable(
                1,
                "shutdown_update_queue_drop",
                Arc::clone(&log),
            ))),
        }),
    ))
    .unwrap();

    let result = handle.inner.shutdown_result();
    let detached_count = handle.inner.detached_queues.lock().len();
    let queue_dropped = log.lock().unwrap().contains(&"shutdown_update_queue_drop");
    drop(handle);
    assert_eq!(result, Err(BlockError::Io));
    assert_eq!(detached_count, 1);
    assert!(
        !queue_dropped,
        "a queue rejected during shutdown installation must remain quarantined"
    );
    assert!(!log.lock().unwrap().contains(&"shutdown_update_queue_drop"));
}

#[test]
fn provisional_group_terminal_waits_for_shared_irq_owner() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let group_owner = Arc::new(GroupOwnerLink::new());
    let handle = BlockDeviceHandle::bootstrap_group_member(
        0,
        String::from("provisional-terminal"),
        Box::new(ProvisionalGroupTerminalController {
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
        }),
        group_owner,
    )
    .unwrap();
    handle
        .inner
        .controller
        .terminal_confirmed
        .store(true, Ordering::Release);

    handle.inner.controller_terminal();

    assert!(
        !log.lock().unwrap().contains(&"queue_shutdown"),
        "a provisional group member must not bypass shared IRQ teardown"
    );
    handle.inner.shutdown_from_controller();
}

#[test]
fn failed_terminal_teardown_quarantines_standalone_irq_registration() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "failed-drop-teardown",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(21)),
        }],
        Box::new(QuiesceFailureController {
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
        }),
    ))
    .unwrap();

    drop(handle);

    assert!(
        !log.lock().unwrap().contains(&"irq_free"),
        "an unsynchronized terminal teardown must retain its IRQ registration"
    );
}

#[test]
fn failed_terminal_teardown_quarantines_group_controller() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let group = BlockGroupHandle {
        name: String::from("failed-group-drop-teardown"),
        controller: IrqMutex::new(Some(Box::new(DropTrackedShutdownFailureGroup {
            log: Arc::clone(&log),
        }))),
        registrations: IrqMutex::new(Vec::new()),
        members: Vec::new(),
        teardown_state: AtomicU8::new(GROUP_RUNNING),
        teardown_waiters: TaskWaiters::new(),
    };

    drop(group);

    assert!(
        !log.lock().unwrap().contains(&"group_controller_drop"),
        "an unconfirmed group controller must remain quarantined"
    );
}

#[test]
fn partial_group_irq_enable_with_failed_synchronize_quarantines_all_owners() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .next_registration
        .store(0, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(1, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(true, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let member = GroupMemberController {
        name: "partial-enable-member",
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
        terminal_on_rearm: false,
        rearm_count: 0,
    };
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "partial-enable-group",
            [
                BlockIrqSource {
                    source_id: 0,
                    irq: IrqId::new(IrqDomainId(1), HwIrq(22)),
                },
                BlockIrqSource {
                    source_id: 1,
                    irq: IrqId::new(IrqDomainId(1), HwIrq(23)),
                },
            ],
            Box::new(TwoIrqControllerGroup {
                members: Some(vec![BlockGroupMember::new(0, Box::new(member))]),
                log: Arc::clone(&log),
            }),
        )],
    );
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(usize::MAX, Ordering::Release);

    assert!(runtime.devices().is_empty());
    let log = log.lock().unwrap();
    assert!(log.contains(&"irq_enable_failed"));
    assert!(!log.contains(&"irq_free"));
    assert!(!log.contains(&"member_shutdown"));
    assert!(!log.contains(&"group_shutdown"));
    assert!(!log.contains(&"queue_shutdown"));
}

#[test]
fn member_shutdown_failure_quarantines_unstopped_group_controller() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let member = GroupMemberShutdownFailureController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
    };
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "member-shutdown-failure-group",
            [BlockIrqSource {
                source_id: 0,
                irq: IrqId::new(IrqDomainId(1), HwIrq(24)),
            }],
            Box::new(DropTrackedMemberFailureGroup {
                members: Some(vec![BlockGroupMember::new(0, Box::new(member))]),
                log: Arc::clone(&log),
            }),
        )],
    );

    assert_eq!(runtime.release_irqs_for_passthrough(), Err(BlockError::Io));
    drop(runtime);

    assert!(
        !log.lock().unwrap().contains(&"group_controller_drop"),
        "member shutdown failure must retain the unstopped group controller"
    );
}

#[test]
fn failed_irq_registration_stops_controller_before_dropping_emitted_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(true, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
        repeat_device_info_on_quiesce: false,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(11));
    let result = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "failed-registration",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);

    assert_eq!(result.err(), Some(BlkError::Io));
    let log = log.lock().unwrap();
    let failed_registration = log_position(&log, "irq_register_failed");
    let controller = log_position(&log, "controller_shutdown");
    let queue = log_position(&log, "queue_shutdown");
    assert!(failed_registration < controller);
    assert!(controller < queue);
}

#[test]
fn teardown_accepts_repeated_device_info_and_releases_resources_in_order() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
        repeat_device_info_on_quiesce: true,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(9));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "lifecycle",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let hctxs = handle.inner.hctxs.lock().clone();
    let cpu_channels = create_cpu_channels(&hctxs, 8).unwrap();
    assert_eq!(cpu_channels.len(), 8);
    assert!(cpu_channels.iter().all(|channel| channel.hctx.id() == 0));
    for channel in cpu_channels {
        channel.channel.close();
    }

    let shutdown_result = handle.inner.shutdown_result();
    let events = log.lock().unwrap().clone();
    assert_eq!(shutdown_result, Ok(1), "events: {events:?}");
    let log = log.lock().unwrap();
    let quiesce = log_position(&log, "controller_quiesce");
    let disable = log_position(&log, "irq_disable_sync");
    let free = log_position(&log, "irq_free");
    let queue = log_position(&log, "queue_shutdown");
    let controller = log_position(&log, "controller_shutdown");
    assert!(quiesce < disable);
    assert!(disable < free);
    assert!(free < controller);
    assert!(controller < queue);
}

#[test]
fn controller_group_enables_shared_irq_before_unmasking_sources_and_tears_down_once() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let member = |member_name| {
        Box::new(GroupMemberController {
            name: member_name,
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
            terminal_on_rearm: false,
            rearm_count: 0,
        }) as Box<dyn BlockController>
    };
    let group = TestControllerGroup {
        members: Some(vec![
            BlockGroupMember::new(0, member("group-member-0")),
            BlockGroupMember::new(1, member("group-member-1")),
        ]),
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(14));
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "shared-group",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(group),
        )],
    );

    assert_eq!(runtime.devices().len(), 2);
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|entry| **entry == "irq_register_disabled")
            .count(),
        1
    );
    {
        let log = log.lock().unwrap();
        let irq_enable = log_position(&log, "irq_enable");
        assert!(irq_enable < log_position(&log, "member_rearm"));
        assert!(irq_enable < log_position(&log, "group_rearm"));
    }
    assert_eq!(runtime.release_irqs_for_passthrough(), Ok(1));
    let log = log.lock().unwrap();
    assert_eq!(
        log.iter()
            .filter(|entry| **entry == "irq_disable_sync")
            .count(),
        1
    );
    assert_eq!(log.iter().filter(|entry| **entry == "irq_free").count(), 1);
    assert_eq!(
        log.iter()
            .filter(|entry| **entry == "member_shutdown")
            .count(),
        2
    );
    assert!(log_position(&log, "irq_disable_sync") < log_position(&log, "member_shutdown"));
    assert!(log_position(&log, "member_shutdown") < log_position(&log, "group_shutdown"));
}

#[test]
fn group_member_terminal_is_escalated_to_shared_irq_owner() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let member = |member_name, terminal_on_rearm| {
        Box::new(GroupMemberController {
            name: member_name,
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
            terminal_on_rearm,
            rearm_count: 0,
        }) as Box<dyn BlockController>
    };
    let group = TestControllerGroup {
        members: Some(vec![
            BlockGroupMember::new(0, member("terminal-member", true)),
            BlockGroupMember::new(1, member("healthy-member", false)),
        ]),
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(16));
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "terminal-group",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(group),
        )],
    );

    let member = Arc::clone(&runtime.devices[0]);
    assert_eq!(
        member
            .inner
            .controller
            .call(ControllerEvent::Rearm { source_id: 0 }),
        Ok(ControllerState::Shutdown)
    );
    wait_for_group_teardown(&runtime.groups[0]);
    assert!(
        runtime.groups[0].teardown_state.load(Ordering::Acquire) == GROUP_STOPPED,
        "member terminal must be owned by the shared group teardown"
    );
}

#[test]
fn group_member_watchdog_terminal_is_escalated_to_shared_irq_owner() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let member = |member_name| {
        Box::new(GroupMemberController {
            name: member_name,
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
            terminal_on_rearm: false,
            rearm_count: 0,
        }) as Box<dyn BlockController>
    };
    let group = TestControllerGroup {
        members: Some(vec![
            BlockGroupMember::new(0, member("watchdog-member")),
            BlockGroupMember::new(1, member("healthy-member")),
        ]),
        log,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(19));
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "watchdog-terminal-group",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(group),
        )],
    );

    assert_eq!(
        runtime.devices[0]
            .inner
            .controller
            .call(ControllerEvent::Watchdog { queue_id: 0 }),
        Ok(ControllerState::Shutdown)
    );
    let group = &runtime.groups[0];
    wait_for_group_teardown(group);
    assert!(
        group.teardown_state.load(Ordering::Acquire) == GROUP_STOPPED,
        "a watchdog terminal must be owned by the shared group teardown"
    );
}

#[test]
fn irq_synchronize_failure_blocks_hardware_shutdown() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(true, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
        repeat_device_info_on_quiesce: false,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(17));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "irq-sync-failure",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let _ = handle.shutdown();
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    assert!(
        !log.lock().unwrap().contains(&"controller_shutdown"),
        "uncertain IRQ quiescence must prevent hardware shutdown"
    );
}

#[test]
fn closed_submission_channel_is_retryable_only_while_device_is_ready() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::new(StdMutex::new(Vec::new())));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "closed-channel-result",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(18)),
        }],
        Box::new(LifecycleController {
            queue: Some(LifecycleQueue {
                log: Arc::new(StdMutex::new(Vec::new())),
            }),
            log: Arc::new(StdMutex::new(Vec::new())),
            repeat_device_info_on_quiesce: false,
        }),
    ))
    .unwrap();
    let channel = handle.inner.select_cpu_channel().unwrap();
    channel.channel.close();
    let request = OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    };
    let error = match handle.submit_owned(request) {
        Ok(_) => panic!("a closed channel must not accept a flush"),
        Err(error) => error,
    };
    assert_eq!(error.error, BlkError::Retry);

    handle.inner.lifecycle_gate.lock().phase = DevicePhase::Stopping;
    handle.inner.accepting.store(true, Ordering::Release);
    let error = match handle.submit_owned(OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    }) {
        Ok(_) => panic!("a stopping device must not accept a flush"),
        Err(error) => error,
    };
    assert_eq!(error.error, BlkError::Io);
}

#[test]
fn late_hctx_failure_cannot_resurrect_a_stopped_device() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log,
        repeat_device_info_on_quiesce: false,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "late-failure-after-stop",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(handle.shutdown(), 1);
    handle.inner.hctx_failed(0, BlkError::Io);
    assert_eq!(
        handle.inner.state.load(Ordering::Acquire),
        DEVICE_STOPPED,
        "a stale failure notification must not regress terminal device state"
    );
    assert_eq!(
        handle.shutdown(),
        0,
        "terminal teardown must remain idempotent after a stale failure"
    );
}

#[test]
fn teardown_releases_queue_when_quiesce_confirms_prior_watchdog_shutdown() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = TerminalBeforeShutdownController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        terminal: false,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(13));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "terminal-before-shutdown",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(
        handle
            .inner
            .controller
            .call(ControllerEvent::Watchdog { queue_id: 0 }),
        Ok(ControllerState::Shutdown)
    );
    wait_for_device_teardown(&handle.inner);
    assert!(
        log.lock().unwrap().contains(&"queue_shutdown"),
        "a prior terminal acknowledgement must permit queue teardown"
    );
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn controller_can_register_control_irq_before_creating_an_io_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    crate::os::task::reset_test_wait_timeout_count();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let register_retries = Arc::new(AtomicUsize::new(0));
    let controller = EndpointFirstController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        register_retries: Arc::clone(&register_retries),
        log,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(10));

    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "endpoint-first",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(register_retries.load(Ordering::Relaxed), 1);
    assert!(
        crate::os::task::test_wait_timeout_count() >= 1,
        "register retry must sleep on the runtime notification"
    );
    assert_eq!(handle.inner.hctxs.lock().len(), 1);
    assert_eq!(handle.inner.cpu_channels.lock().len(), 1);
    assert_eq!(handle.shutdown(), 1);
}

#[test]
fn bootstrap_preserves_waiting_for_irq_controller_without_io_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let irq = IrqId::new(IrqDomainId(1), HwIrq(15));

    let handle = BlockDeviceHandle::bootstrap(
        String::from("waiting-for-irq"),
        vec![BlockIrqSource { source_id: 0, irq }],
        Box::new(WaitingForIrqController),
        None,
        None,
    )
    .expect("a control IRQ may precede creation of the first I/O queue");

    assert_eq!(handle.inner.state.load(Ordering::Acquire), DEVICE_STARTING);
    assert!(handle.inner.hctxs.lock().is_empty());
    assert_eq!(handle.shutdown(), 1);
}
