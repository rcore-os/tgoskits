use super::*;

#[test]
fn failed_irq_withdrawal_survives_root_binding_destruction() {
    let _test_lock = ORPHAN_QUEUE_TEST_LOCK.lock().unwrap();
    let function_id = DeviceNodeId::new("pending-withdrawal-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
            )
            .with_intx(crate::PciIntxRequirement::new(
                crate::PciIntxPin::A,
                crate::ResourceSlot::new("intx").unwrap(),
            ))
            .unwrap(),
        )
        .unwrap();
    let route = crate::PciIntxRouter::new(
        InterruptControllerId::new(0),
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [16, 17, 18, 19],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    )
    .resolve(&function_id, PciBdf::bus_zero(0), crate::PciIntxPin::A)
    .unwrap();
    builder.set_intx_route(&function_id, route).unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(Arc::clone(&topology))),
    ));
    let recording = Arc::new(RecordingFunction {
        root: Arc::clone(&binding.root),
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
        .bind_registered(
            &function_id,
            DeviceId::new(10),
            recording.clone(),
            &mut grants,
        )
        .unwrap();
    let permit = lease.token.admission.acquire_irq_permit().unwrap();

    drop(lease);
    drop(binding);
    assert_eq!(*recording.withdrawals.lock_irqsave(), 0);

    drop(permit);
    PciRootBinding::retry_orphaned_irq_withdrawals().unwrap();
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
}

#[test]
fn failed_owner_irq_withdrawal_is_retryable() {
    let function_id = DeviceNodeId::new("failed-withdrawal-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
            )
            .with_intx(crate::PciIntxRequirement::new(
                crate::PciIntxPin::A,
                crate::ResourceSlot::new("intx").unwrap(),
            ))
            .unwrap(),
        )
        .unwrap();
    let route = crate::PciIntxRouter::new(
        InterruptControllerId::new(0),
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [16, 17, 18, 19],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    )
    .resolve(&function_id, PciBdf::bus_zero(0), crate::PciIntxPin::A)
    .unwrap();
    builder.set_intx_route(&function_id, route).unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(Arc::clone(&topology))),
    ));
    let recording = Arc::new(RecordingFunction {
        root: Arc::clone(&binding.root),
        bdf,
        reads: SpinLock::new(Vec::new()),
        writes: SpinLock::new(Vec::new()),
        commands: SpinLock::new(Vec::new()),
        resets: SpinLock::new(Vec::new()),
        reset_failures: SpinLock::new(0),
        withdrawals: SpinLock::new(0),
        withdraw_failures: SpinLock::new(1),
        irq_line: None,
        supports_effects: false,
        pending: false,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(11),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    drop(lease);
    assert_eq!(*recording.withdrawals.lock_irqsave(), 0);
    assert!(binding.retry_irq_withdrawals().is_ok());
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
}

#[test]
fn binding_initially_synchronizes_the_current_command_state() {
    let function_id = DeviceNodeId::new("initial-command-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    root.write_config(
        bdf,
        ConfigOffset::new(4).unwrap(),
        AccessWidth::Word,
        0x0406,
    )
    .unwrap();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::clone(&root),
    ));
    let recording = Arc::new(RecordingFunction {
        root,
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
    let _lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(7),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    let commands = recording.commands.lock_irqsave();
    assert_eq!(commands.len(), 1);
    assert!(commands[0].0.memory_space_enable());
    assert!(commands[0].0.bus_master_enable());
    assert!(commands[0].0.interrupt_disable());
    assert_eq!(commands[0].1, DeviceId::new(7));
}

#[test]
fn binding_rolls_back_route_and_grant_when_initial_sync_fails() {
    let function_id = DeviceNodeId::new("initial-sync-failure-endpoint").unwrap();
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

    assert!(matches!(
        binding.bind_registered(
            &function_id,
            DeviceId::new(7),
            Arc::new(StubFunction { fail_command: true }),
            &mut grants,
        ),
        Err(DeviceManagerError::Device(DeviceError::Unsupported { .. }))
    ));
    assert!(grants.is_empty());
    assert!(binding.router.state.lock_irqsave().endpoints.is_empty());
}

#[test]
fn command_callback_failure_keeps_the_root_owned_command_commit() {
    let function_id = DeviceNodeId::new("failing-command-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let function = Arc::new(ToggleCommandFunction {
        fail_command: AtomicBool::new(false),
    });
    let mut grants = Vec::new();
    let _lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(8),
            function.clone(),
            &mut grants,
        )
        .unwrap();

    function.fail_command.store(true, Ordering::Release);

    assert!(matches!(
        binding.write_config(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x0406
        ),
        Err(DeviceError::Unsupported { .. })
    ));
    assert_eq!(
        binding
            .read_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word)
            .unwrap(),
        0x0406
    );
}
