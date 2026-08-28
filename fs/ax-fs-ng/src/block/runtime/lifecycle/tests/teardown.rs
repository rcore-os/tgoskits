use super::*;

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

    assert_eq!(handle.shutdown(), 1);
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
    assert_eq!(runtime.release_irqs_for_passthrough(), 1);
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
    assert_eq!(handle.shutdown(), 1);
    assert!(
        log.lock().unwrap().contains(&"queue_shutdown"),
        "a prior terminal acknowledgement must permit queue teardown"
    );
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
    )
    .expect("a control IRQ may precede creation of the first I/O queue");

    assert_eq!(handle.inner.state.load(Ordering::Acquire), DEVICE_STARTING);
    assert!(handle.inner.hctxs.lock().is_empty());
    assert_eq!(handle.shutdown(), 1);
}
