use super::*;

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
fn teardown_disables_controller_before_queue_memory_is_released() {
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
