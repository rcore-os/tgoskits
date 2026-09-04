use super::*;
use crate::{PciBarIndex, PciMemoryBar, ResourceRequest};

#[test]
fn withdrawal_operation_reports_busy_lifecycle_without_spinning() {
    let function_id = DeviceNodeId::new("deferred-withdrawal-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(5),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    let reset = binding.begin_reset_operation().unwrap();

    drop(lease);
    assert_eq!(
        binding
            .lifecycle
            .lock_irqsave()
            .pending_withdrawals
            .as_slice(),
        &[DeviceId::new(5)]
    );
    reset.finish_reset().unwrap();
}

#[test]
fn reset_reclaims_lease_dropped_during_completion_handoff() {
    let function_id = DeviceNodeId::new("handoff-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let recording = Arc::new(RecordingFunction {
        root,
        bdf: topology.function(&function_id).unwrap().bdf(),
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: None,
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(&function_id, DeviceId::new(13), recording, &mut grants)
        .unwrap();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    binding.set_reset_handoff_hook({
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Arc::new(move || {
            entered.wait();
            release.wait();
        })
    });
    let defer_entered = Arc::new(std::sync::Barrier::new(2));
    let defer_release = Arc::new(std::sync::Barrier::new(2));
    binding.set_deferred_withdrawal_hook({
        let entered = Arc::clone(&defer_entered);
        let release = Arc::clone(&defer_release);
        Arc::new(move || {
            entered.wait();
            release.wait();
        })
    });

    let reset_binding = Arc::clone(&binding);
    let reset_thread = std::thread::spawn(move || reset_binding.reset_lifecycle());
    entered.wait();
    let drop_thread = std::thread::spawn(move || drop(lease));
    defer_entered.wait();
    release.wait();
    defer_release.wait();

    assert!(reset_thread.join().unwrap().is_ok());
    drop_thread.join().unwrap();
    assert!(
        binding
            .lifecycle
            .lock_irqsave()
            .pending_withdrawals
            .is_empty()
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .contains_key(&DeviceId::new(13))
    );
}

#[test]
fn lifecycle_reset_advances_only_the_admission_epoch() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let old = router
        .activate(DeviceId::new(5), function)
        .expect("test route activation succeeds");
    let old_grant = old.grant(false);
    let replacements = router.reset_admissions().unwrap();
    assert_eq!(replacements.len(), 1);
    assert!(matches!(
        old.admission.clone().acquire(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(!old_grant.admission_is_open());

    router.open_admissions();
    let (_, fresh) = &replacements[0];
    assert_eq!(fresh.binding_generation(), old.binding_generation());
    assert_eq!(fresh.admission_epoch(), old.admission_epoch() + 1);
    assert!(router.endpoint(fresh).is_ok());
    assert!(fresh.admission.clone().acquire(fresh).is_ok());
    assert!(fresh.grant(false).admission_is_open());
}

#[test]
fn full_reset_rejects_config_snapshot_after_admission_close() {
    let ConfigEffectRouteFixture {
        binding,
        root,
        bdf,
        effect_offset,
        recording: _,
        lease,
    } = config_effect_route_fixture(0);
    let closed = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    binding.router.set_reset_admission_hook({
        let closed = Arc::clone(&closed);
        let release = Arc::clone(&release);
        Arc::new(move || {
            closed.wait();
            release.wait();
        })
    });

    let reset_binding = Arc::clone(&binding);
    let reset_thread = std::thread::spawn(move || reset_binding.reset_lifecycle());
    closed.wait();

    assert!(matches!(
        root.prepare_read_config(bdf, effect_offset, AccessWidth::Dword),
        Err(PciError::ConfigEffectUnavailable { .. })
    ));

    release.wait();
    assert!(reset_thread.join().unwrap().is_ok());

    let (fresh, _, effect) = config_effect_snapshot(&root, bdf, effect_offset);
    assert_eq!(fresh.admission_epoch(), 2);
    assert_eq!(effect.effect(), PciConfigEffectId::new(7));
    let fresh_lease = binding.router.lease(&fresh, false).unwrap();
    drop(fresh_lease);
    drop(lease);
}

#[test]
fn full_reset_stales_a_route_snapshot_captured_before_close() {
    let ConfigEffectRouteFixture {
        binding,
        root,
        bdf,
        effect_offset,
        recording: _,
        lease,
    } = config_effect_route_fixture(0);
    let reset_closed = Arc::new(std::sync::Barrier::new(2));
    let reset_release = Arc::new(std::sync::Barrier::new(2));
    binding.router.set_reset_admission_hook({
        let reset_closed = Arc::clone(&reset_closed);
        let reset_release = Arc::clone(&reset_release);
        Arc::new(move || {
            reset_closed.wait();
            reset_release.wait();
        })
    });

    let snapshot_ready = Arc::new(std::sync::Barrier::new(2));
    let snapshot_release = Arc::new(std::sync::Barrier::new(2));
    let snapshot_root = Arc::clone(&root);
    let snapshot_ready_thread = Arc::clone(&snapshot_ready);
    let snapshot_release_thread = Arc::clone(&snapshot_release);
    let snapshot_thread = std::thread::spawn(move || {
        let snapshot = config_effect_snapshot(&snapshot_root, bdf, effect_offset);
        snapshot_ready_thread.wait();
        snapshot_release_thread.wait();
        snapshot
    });
    snapshot_ready.wait();

    let reset_binding = Arc::clone(&binding);
    let reset_thread = std::thread::spawn(move || reset_binding.reset_lifecycle());
    reset_closed.wait();
    snapshot_release.wait();
    reset_release.wait();

    let (old, old_command, old_effect) = snapshot_thread.join().unwrap();
    assert!(reset_thread.join().unwrap().is_ok());
    assert_eq!(old.binding_generation(), 1);
    assert_eq!(old.admission_epoch(), 1);
    assert_eq!(old_effect.effect(), PciConfigEffectId::new(7));
    assert_eq!(old_effect.command(), old_command);
    assert_eq!(old_effect.capability_snapshot().bytes()[2], 0x11);
    assert!(matches!(
        binding.router.endpoint(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(matches!(
        old.admission.clone().acquire(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(!old.grant(false).admission_is_open());

    let (fresh, ..) = config_effect_snapshot(&root, bdf, effect_offset);
    assert_eq!(fresh.binding_generation(), old.binding_generation());
    assert_eq!(fresh.admission_epoch(), old.admission_epoch() + 1);
    let fresh_lease = binding.router.lease(&fresh, false).unwrap();
    drop(fresh_lease);
    drop(lease);
}

#[test]
fn full_reset_drains_an_admitted_route_lease_before_endpoint_reset() {
    let ConfigEffectRouteFixture {
        binding,
        root,
        bdf,
        effect_offset,
        recording,
        lease,
    } = config_effect_route_fixture(0);
    let (old, ..) = config_effect_snapshot(&root, bdf, effect_offset);
    let scoped = binding.router.lease(&old, false).unwrap();
    let scoped_grant = scoped.grant.clone();
    let drain_observed = Arc::new(std::sync::Barrier::new(2));
    let drain_release = Arc::new(std::sync::Barrier::new(2));
    old.admission.set_drain_observed_hook({
        let drain_observed = Arc::clone(&drain_observed);
        let drain_release = Arc::clone(&drain_release);
        Arc::new(move || {
            drain_observed.wait();
            drain_release.wait();
        })
    });

    let reset_binding = Arc::clone(&binding);
    let reset_thread = std::thread::spawn(move || reset_binding.reset_lifecycle());
    drain_observed.wait();

    assert!(matches!(
        root.prepare_read_config(bdf, effect_offset, AccessWidth::Dword),
        Err(PciError::ConfigEffectUnavailable { .. })
    ));
    assert!(recording.resets.lock_irqsave().is_empty());

    let mut runtime = crate::DeviceRuntime::empty();
    runtime.with_routed_grant_for_test(0, scoped_grant.clone(), |context| {
        let mut callback = |_nested: &mut dyn DeviceContext| Ok(());
        context
            .with_routed_device(&scoped_grant, &mut callback)
            .unwrap();
    });
    drop(scoped);
    drain_release.wait();

    assert!(reset_thread.join().unwrap().is_ok());
    assert_eq!(recording.resets.lock_irqsave().len(), 1);
    assert!(matches!(
        old.admission.clone().acquire(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    let (fresh, ..) = config_effect_snapshot(&root, bdf, effect_offset);
    let fresh_lease = binding.router.lease(&fresh, false).unwrap();
    drop(fresh_lease);
    drop(lease);
}

#[test]
fn full_reset_failure_after_admission_barrier_stays_fail_closed() {
    let ConfigEffectRouteFixture {
        binding,
        root,
        bdf,
        effect_offset,
        recording,
        lease,
    } = config_effect_route_fixture(1);
    let (old, ..) = config_effect_snapshot(&root, bdf, effect_offset);
    let closed = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    binding.router.set_reset_admission_hook({
        let closed = Arc::clone(&closed);
        let release = Arc::clone(&release);
        Arc::new(move || {
            closed.wait();
            release.wait();
        })
    });

    let reset_binding = Arc::clone(&binding);
    let reset_thread = std::thread::spawn(move || reset_binding.reset_lifecycle());
    closed.wait();
    assert!(matches!(
        root.prepare_read_config(bdf, effect_offset, AccessWidth::Dword),
        Err(PciError::ConfigEffectUnavailable { .. })
    ));
    release.wait();

    assert!(matches!(
        reset_thread.join().unwrap(),
        Err(DeviceManagerError::Device(DeviceError::Backend { .. }))
    ));
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::ResetFailed
    );
    assert!(matches!(
        binding.router.endpoint(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(matches!(
        old.admission.clone().acquire(&old),
        Err(DeviceError::InvalidState { .. })
    ));
    let current = binding
        .router
        .state
        .lock_irqsave()
        .endpoints
        .get(&DeviceId::new(27))
        .unwrap()
        .token
        .clone();
    assert_eq!(current.binding_generation(), old.binding_generation());
    assert_eq!(current.admission_epoch(), old.admission_epoch() + 1);
    assert!(!current.grant(false).admission_is_open());
    assert!(binding.router.lease(&current, false).is_err());
    assert_eq!(recording.resets.lock_irqsave().len(), 1);

    drop(lease);
    binding
        .begin_stop_operation()
        .finish_stop()
        .expect("teardown remains available from ResetFailed");
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Dead
    );
}

#[test]
fn full_lifecycle_reset_resets_endpoint_before_reopening_admission() {
    let function_id = DeviceNodeId::new("resettable-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let sink = Arc::new(FailingDeassertSink {
        fail_deassert: AtomicBool::new(false),
        asserted: AtomicBool::new(false),
    });
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(19),
        InterruptTrigger::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    line.assert().unwrap();
    let recording = Arc::new(RecordingFunction {
        root,
        bdf: topology.function(&function_id).unwrap().bdf(),
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: Some(line),
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(7),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    binding.reset_lifecycle().unwrap();

    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
    assert!(!sink.asserted.load(Ordering::Relaxed));

    let resets = recording.resets.lock_irqsave();
    assert_eq!(resets.len(), 1);
    assert!(!resets[0].bus_master_enable());
    drop(resets);
    let token = binding
        .router
        .state
        .lock_irqsave()
        .endpoints
        .get(&DeviceId::new(7))
        .unwrap()
        .token
        .clone();
    assert_eq!(token.binding_generation(), lease.token.binding_generation());
    assert_eq!(token.admission_epoch(), 2);
    assert!(token.admission.clone().acquire(&token).is_ok());
}

#[test]
fn full_lifecycle_reset_failure_keeps_endpoint_admission_closed() {
    let function_id = DeviceNodeId::new("unresettable-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let sink = Arc::new(FailingDeassertSink {
        fail_deassert: AtomicBool::new(false),
        asserted: AtomicBool::new(false),
    });
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(19),
        InterruptTrigger::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    line.assert().unwrap();
    let recording = Arc::new(RecordingFunction {
        root,
        bdf: topology.function(&function_id).unwrap().bdf(),
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(1),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: Some(line),
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(7),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    assert!(matches!(
        binding.reset_lifecycle(),
        Err(DeviceManagerError::Device(DeviceError::Backend { .. }))
    ));
    assert_eq!(recording.resets.lock_irqsave().len(), 1);
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
    assert!(!sink.asserted.load(Ordering::Relaxed));
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::ResetFailed
    );
    let token = binding
        .router
        .state
        .lock_irqsave()
        .endpoints
        .get(&DeviceId::new(7))
        .unwrap()
        .token
        .clone();
    assert!(!token.grant(false).admission_is_open());
    assert!(matches!(
        token.admission.clone().acquire(&token),
        Err(DeviceError::InvalidState { .. })
    ));
    drop(lease);
}

#[test]
fn reset_irq_cleanup_failure_stays_closed_until_teardown_retries_withdrawal() {
    let function_id = DeviceNodeId::new("reset-cleanup-failure-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let sink = Arc::new(FailingDeassertSink {
        fail_deassert: AtomicBool::new(false),
        asserted: AtomicBool::new(false),
    });
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(19),
        InterruptTrigger::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    line.assert().unwrap();
    let recording = Arc::new(RecordingFunction {
        root,
        bdf: topology.function(&function_id).unwrap().bdf(),
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(1),
        irq_line: Some(line),
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(12),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    assert!(matches!(
        binding.reset_lifecycle(),
        Err(DeviceManagerError::Device(DeviceError::Backend { .. }))
    ));
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::ResetFailed
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .get(&DeviceId::new(12))
            .unwrap()
            .token
            .grant(false)
            .admission_is_open()
    );
    assert!(sink.asserted.load(Ordering::Relaxed));

    *recording.withdraw_failures.lock_irqsave() = 0;
    drop(lease);
    assert!(!sink.asserted.load(Ordering::Relaxed));
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
    drop(binding);
}

#[test]
fn binding_callback_can_reenter_lifecycle_without_holding_its_lock() {
    let function_id = DeviceNodeId::new("reentrant-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let function = Arc::new(ReentrantLifecycleFunction {
        binding: Arc::downgrade(&binding),
    });
    let mut grants = Vec::new();

    let lease = binding
        .bind_registered(&function_id, DeviceId::new(7), function, &mut grants)
        .unwrap();
    drop(lease);
}

#[test]
fn endpoint_binding_rejects_a_busy_lifecycle_owner_gate() {
    let function_id = DeviceNodeId::new("gated-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let gate = binding.begin_binding_operation().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let bind_binding = Arc::clone(&binding);
    std::thread::spawn(move || {
        let mut grants = Vec::new();
        let result = bind_binding.bind_registered(
            &function_id,
            DeviceId::new(7),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        );
        sender.send(result.is_ok()).unwrap();
    });

    assert!(
        !receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
    );
    gate.finish_restore().unwrap();
}

#[test]
fn root_rejects_a_second_binding_for_the_same_function() {
    use crate::{PciClass, PciEndpointIdentity, PciFunctionSpec, PciTopologyBuilder};

    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            DeviceNodeId::new("endpoint").unwrap(),
            PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = PciRootState::new(Arc::clone(&topology));
    let function_id = DeviceNodeId::new("endpoint").unwrap();

    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let first = router
        .activate(DeviceId::new(1), Arc::clone(&function))
        .unwrap();
    root.reserve_endpoint_binding(&function_id)
        .unwrap()
        .commit(first.clone())
        .unwrap();
    assert!(matches!(
        root.reserve_endpoint_binding(&function_id),
        Err(PciError::FunctionAlreadyBound { .. })
    ));

    // Unbind invalidates the route; the same token never revives.
    drop(router.invalidate(&first));
    root.unbind_route_for_binding(&first);
    assert_eq!(root.resolve_bound_bar(0xc000_0000, AccessWidth::Byte), None);
    let second = router
        .activate(DeviceId::new(1), Arc::clone(&function))
        .unwrap();
    root.reserve_endpoint_binding(&function_id)
        .unwrap()
        .commit(second)
        .unwrap();
}

#[test]
fn reset_completion_cannot_open_a_second_reset_epoch() {
    let function_id = DeviceNodeId::new("reset-owner-race-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(17),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();

    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    binding.set_admission_open_hook({
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Arc::new(move || {
            entered.wait();
            release.wait();
        })
    });

    let first_binding = Arc::clone(&binding);
    let first = std::thread::spawn(move || {
        first_binding
            .begin_reset_operation()
            .unwrap()
            .finish_reset()
    });
    entered.wait();

    let (sender, receiver) = std::sync::mpsc::channel();
    let second_binding = Arc::clone(&binding);
    std::thread::spawn(move || {
        sender.send(second_binding.reset_lifecycle()).unwrap();
    });
    let second_result = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("second reset reaches the old completion window");
    assert!(second_result.is_err());

    release.wait();
    assert!(first.join().unwrap().is_ok());
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Running
    );
    let token = binding
        .router
        .state
        .lock_irqsave()
        .endpoints
        .get(&DeviceId::new(17))
        .unwrap()
        .token
        .clone();
    assert_eq!(token.admission_epoch(), 1);
    assert!(token.grant(false).admission_is_open());
    drop(lease);
}

#[test]
fn stop_request_does_not_overwrite_an_active_lifecycle_owner() {
    let binding = PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(Arc::new(
            PciTopologyBuilder::new()
                .resolve(0xc000_0000..0xc100_0000)
                .unwrap(),
        ))),
    );
    let reset = binding.begin_reset_operation().unwrap();
    let stop = binding.begin_stop_operation();

    // Keep the operations alive while checking the handoff metadata. The
    // fixed implementation returns an unclaimed successor stop operation;
    // dropping either operation here would otherwise complete it out of order.
    core::mem::forget(reset);
    core::mem::forget(stop);
    let owner_is_reset = binding.lifecycle_owner_is_reset();
    let stop_requested = binding.stop_requested();
    core::mem::forget(binding);
    assert!(owner_is_reset);
    assert!(stop_requested);
}

#[test]
fn stop_request_supersedes_reset_before_admission_publication() {
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(Arc::new(
            PciTopologyBuilder::new()
                .resolve(0xc000_0000..0xc100_0000)
                .unwrap(),
        ))),
    ));
    binding.set_completion_closing_hook({
        let binding = Arc::clone(&binding);
        Arc::new(move || {
            // The stop request is made after reset has sealed its owner but
            // before publication is reserved. It must become a successor,
            // rather than replacing the reset owner in place.
            core::mem::forget(binding.begin_stop_operation());
        })
    });

    assert!(matches!(
        binding.reset_lifecycle(),
        Err(DeviceManagerError::InvalidState { .. })
    ));
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Stopping
    );

    binding.begin_stop_operation().finish_stop().unwrap();
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Dead
    );
}

#[test]
fn stop_successor_claims_a_withdrawal_queued_after_reset_completion() {
    let function_id = DeviceNodeId::new("stop-successor-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(25),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    binding.set_completion_closing_hook({
        let binding = Arc::clone(&binding);
        Arc::new(move || {
            core::mem::forget(binding.begin_stop_operation());
        })
    });

    assert!(binding.reset_lifecycle().is_err());
    // The reset has handed off to a pending stop, but no stop owner has
    // claimed the slot yet. A lease may still drop in this final window.
    drop(lease);

    binding.begin_stop_operation().finish_stop().unwrap();
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Dead
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .contains_key(&DeviceId::new(25))
    );
}

#[test]
fn binding_completion_drains_a_concurrent_lease_drop() {
    let first_id = DeviceNodeId::new("binding-owner-endpoint").unwrap();
    let second_id = DeviceNodeId::new("binding-drop-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    for function_id in [&first_id, &second_id] {
        builder
            .add_function(PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
            ))
            .unwrap();
    }
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let mut grants = Vec::new();
    let dropped = binding
        .bind_registered(
            &second_id,
            DeviceId::new(18),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    let dropped_grant = dropped.token.grant(false);

    let dropped_lease = Arc::new(SpinLock::new(Some(dropped)));
    binding.set_completion_closing_hook({
        let dropped_lease = Arc::clone(&dropped_lease);
        Arc::new(move || {
            drop(dropped_lease.lock_irqsave().take());
        })
    });
    let operation = binding.begin_binding_operation().unwrap();
    operation.finish_restore().unwrap();

    assert!(
        binding
            .lifecycle
            .lock_irqsave()
            .pending_withdrawals
            .is_empty()
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .contains_key(&DeviceId::new(18))
    );
    assert!(!dropped_grant.admission_is_open());
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Running
    );
    let _ = first_id;
}

#[test]
fn withdrawal_completion_drains_a_last_window_lease_drop() {
    let first_id = DeviceNodeId::new("withdraw-owner-endpoint").unwrap();
    let second_id = DeviceNodeId::new("withdraw-drop-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    for function_id in [&first_id, &second_id] {
        builder
            .add_function(PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
            ))
            .unwrap();
    }
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let mut grants = Vec::new();
    let first = binding
        .bind_registered(
            &first_id,
            DeviceId::new(19),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    let second = binding
        .bind_registered(
            &second_id,
            DeviceId::new(20),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    let second_grant = second.token.grant(false);

    let second_lease = Arc::new(SpinLock::new(Some(second)));
    binding.set_completion_closing_hook({
        let second_lease = Arc::clone(&second_lease);
        Arc::new(move || {
            drop(second_lease.lock_irqsave().take());
        })
    });
    let operation = binding.begin_withdrawal_operation().unwrap();
    binding.withdraw_endpoint(DeviceId::new(19)).unwrap();
    operation.finish_restore().unwrap();

    assert!(
        binding
            .lifecycle
            .lock_irqsave()
            .pending_withdrawals
            .is_empty()
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .contains_key(&DeviceId::new(19))
    );
    assert!(
        !binding
            .router
            .state
            .lock_irqsave()
            .endpoints
            .contains_key(&DeviceId::new(20))
    );
    assert!(!second_grant.admission_is_open());
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Running
    );
    drop(first);
}

#[test]
fn lease_drop_withdraws_root_route_before_lifecycle_owner_is_available() {
    let function_id = DeviceNodeId::new("route-first-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
            )
            .with_bar(
                PciMemoryBar::new(PciBarIndex::new(0).unwrap(), 0x1000)
                    .unwrap()
                    .with_address(ResourceRequest::Fixed(0xc000_0000)),
            )
            .unwrap(),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(21),
            Arc::new(StubFunction {
                fail_command: false,
            }),
            &mut grants,
        )
        .unwrap();
    root.write_config(
        topology.function(&function_id).unwrap().bdf(),
        ConfigOffset::new(4).unwrap(),
        AccessWidth::Word,
        0x0406,
    )
    .unwrap();
    let bar_address = topology
        .function(&function_id)
        .unwrap()
        .bar(PciBarIndex::new(0).unwrap())
        .unwrap()
        .address();
    assert_eq!(
        root.read_config(
            topology.function(&function_id).unwrap().bdf(),
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
        )
        .unwrap(),
        0x0406
    );
    let operation = binding.begin_binding_operation().unwrap();

    assert!(
        root.resolve_bound_bar(bar_address, AccessWidth::Byte)
            .is_some()
    );
    drop(lease);
    assert!(
        root.resolve_bound_bar(bar_address, AccessWidth::Byte)
            .is_none()
    );

    operation.finish_restore().unwrap();
}

#[test]
fn late_withdrawal_failure_does_not_rollback_published_reset() {
    let function_id = DeviceNodeId::new("late-withdrawal-failure-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
            )
            .with_bar(
                PciMemoryBar::new(PciBarIndex::new(0).unwrap(), 0x1000)
                    .unwrap()
                    .with_address(ResourceRequest::Fixed(0xc000_0000)),
            )
            .unwrap(),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let recording = Arc::new(RecordingFunction {
        root: Arc::clone(&root),
        bdf: topology.function(&function_id).unwrap().bdf(),
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: None,
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(23),
            recording.clone(),
            &mut grants,
        )
        .unwrap();
    let lease = Arc::new(SpinLock::new(Some(lease)));
    binding.set_admission_open_hook({
        let lease = Arc::clone(&lease);
        let recording = Arc::clone(&recording);
        Arc::new(move || {
            *recording.withdraw_failures.lock_irqsave() = 1;
            drop(lease.lock_irqsave().take());
        })
    });

    assert!(binding.reset_lifecycle().is_ok());
    assert_eq!(
        binding.lifecycle.lock_irqsave().state,
        BindingLifecycleState::Running
    );
    assert!(root.resolve_bar(0xc000_0000, AccessWidth::Byte).is_none());
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);

    binding.retry_irq_withdrawals().unwrap();
    assert_eq!(*recording.withdrawals.lock_irqsave(), 2);
}

#[test]
fn old_lease_retracts_the_current_root_route_after_epoch_replacement() {
    let function_id = DeviceNodeId::new("epoch-route-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
            )
            .with_bar(
                PciMemoryBar::new(PciBarIndex::new(0).unwrap(), 0x1000)
                    .unwrap()
                    .with_address(ResourceRequest::Fixed(0xc000_0000)),
            )
            .unwrap(),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let bdf = topology.function(&function_id).unwrap().bdf();
    let bar_address = topology
        .function(&function_id)
        .unwrap()
        .bar(PciBarIndex::new(0).unwrap())
        .unwrap()
        .address();
    let recording = Arc::new(RecordingFunction {
        root: Arc::clone(&root),
        bdf,
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(0),
        irq_line: None,
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(&function_id, DeviceId::new(24), recording, &mut grants)
        .unwrap();
    root.write_config(
        bdf,
        ConfigOffset::new(4).unwrap(),
        AccessWidth::Word,
        0x0406,
    )
    .unwrap();
    binding.reset_lifecycle().unwrap();
    root.write_config(
        bdf,
        ConfigOffset::new(4).unwrap(),
        AccessWidth::Word,
        0x0002,
    )
    .unwrap();
    assert!(
        root.resolve_bound_bar(bar_address, AccessWidth::Byte)
            .is_some()
    );

    drop(lease);
    assert!(
        root.resolve_bound_bar(bar_address, AccessWidth::Byte)
            .is_none()
    );
}
