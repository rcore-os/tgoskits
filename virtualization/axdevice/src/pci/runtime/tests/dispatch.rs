use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use axdevice_base::{Device, DeviceAccess, DeviceContext, DeviceError, Resource};

use super::*;

struct ReverseCommandFunction {
    first_entered: Arc<Barrier>,
    first_release: Arc<Barrier>,
    second_finished: Arc<Barrier>,
    first_started: AtomicBool,
    callbacks: SpinLock<Vec<PciCommandState>>,
    completions: SpinLock<Vec<PciCommandState>>,
    applied: SpinLock<Option<PciCommandState>>,
}

impl Device for ReverseCommandFunction {
    fn name(&self) -> &str {
        "reverse-command-test-function"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

impl PciFunction for ReverseCommandFunction {
    fn read_bar(
        &self,
        _access: PciBarAccess,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        Ok(())
    }

    fn command_changed(
        &self,
        command: PciCommandState,
        _context: &mut dyn PciEndpointContext,
    ) -> DeviceResult {
        self.callbacks.lock_irqsave().push(command);
        let mut applied = self.applied.lock_irqsave();
        if applied.is_none_or(|previous| command.revision() > previous.revision()) {
            *applied = Some(command);
        }
        drop(applied);
        if command.interrupt_disable() {
            self.first_started.store(true, Ordering::Release);
            self.first_entered.wait();
            self.first_release.wait();
        } else if self.first_started.load(Ordering::Acquire) {
            self.second_finished.wait();
        }
        self.completions.lock_irqsave().push(command);
        Ok(())
    }
}

#[test]
fn command_callbacks_can_complete_in_reverse_order_without_stale_state() {
    let function_id = DeviceNodeId::new("reverse-command-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(PciFunctionSpec::new(
            function_id.clone(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0x01, 0x80, 0)),
        ))
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let root = Arc::new(PciRootState::new(topology));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("reverse-command-host").unwrap(),
        root,
    ));
    let function = Arc::new(ReverseCommandFunction {
        first_entered: Arc::new(Barrier::new(2)),
        first_release: Arc::new(Barrier::new(2)),
        second_finished: Arc::new(Barrier::new(2)),
        first_started: AtomicBool::new(false),
        callbacks: SpinLock::new(Vec::new()),
        completions: SpinLock::new(Vec::new()),
        applied: SpinLock::new(None),
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(31),
            function.clone(),
            &mut grants,
        )
        .unwrap();
    function.callbacks.lock_irqsave().clear();
    function.completions.lock_irqsave().clear();

    let first_binding = Arc::clone(&binding);
    let first = thread::spawn(move || {
        first_binding
            .write_config(
                bdf,
                ConfigOffset::new(4).unwrap(),
                AccessWidth::Word,
                0x0400,
            )
            .unwrap();
    });
    function.first_entered.wait();

    let second_binding = Arc::clone(&binding);
    let second = thread::spawn(move || {
        second_binding
            .write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 0)
            .unwrap();
    });
    function.second_finished.wait();
    second.join().unwrap();
    function.first_release.wait();
    first.join().unwrap();

    let callbacks = function.callbacks.lock_irqsave();
    assert_eq!(callbacks.len(), 2);
    assert!(callbacks[0].interrupt_disable());
    assert!(!callbacks[1].interrupt_disable());
    assert!(callbacks[0].revision() < callbacks[1].revision());
    let completions = function.completions.lock_irqsave();
    assert_eq!(completions.len(), 2);
    assert!(!completions[0].interrupt_disable());
    assert!(completions[1].interrupt_disable());
    assert!(
        !function
            .applied
            .lock_irqsave()
            .as_ref()
            .unwrap()
            .interrupt_disable()
    );
    drop(lease);
}

#[test]
fn binding_dispatches_config_effects_and_command_transitions() {
    let effect = PciCapabilityEffectRegion::new(
        PciConfigEffectId::new(7),
        8,
        6,
        PciCapabilityEffectAccess::ReadWrite,
    )
    .unwrap();
    let capability = PciCapabilitySpec::new(
        PciCapabilityId::new(9),
        alloc::vec![0, 0, 0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0, 0, 0, 0, 0,],
        alloc::vec![0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap()
    .with_effect(effect)
    .unwrap();
    let function_id = DeviceNodeId::new("effect-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
            )
            .with_capability(capability),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
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
        supports_effects: true,
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
    let capability_offset = topology
        .function(&function_id)
        .unwrap()
        .capabilities()
        .next()
        .unwrap()
        .offset()
        .value();

    // Selector bytes are ordinary root-owned storage. The effect must
    // observe their value captured by the same transaction.
    binding
        .write_config(
            bdf,
            ConfigOffset::new(capability_offset + 4).unwrap(),
            AccessWidth::Dword,
            0x6655_4433,
        )
        .unwrap();
    assert!(recording.reads.lock_irqsave().is_empty());
    assert!(recording.writes.lock_irqsave().is_empty());

    assert_eq!(
        binding
            .read_config(
                bdf,
                ConfigOffset::new(capability_offset + 8).unwrap(),
                AccessWidth::Dword,
            )
            .unwrap(),
        0x5a
    );
    let read = recording.reads.lock_irqsave().pop().unwrap();
    assert_eq!(read.0.capability(), PciCapabilityId::new(9));
    assert_eq!(read.0.effect(), PciConfigEffectId::new(7));
    assert_eq!(read.0.offset(), 8);
    assert_eq!(read.0.width(), AccessWidth::Dword);
    assert_eq!(read.1, DeviceId::new(7));
    assert_eq!(read.2, 0x1041_1af4);
    assert_eq!(
        &read.0.capability_snapshot().bytes()[..8],
        &[0, 0, 0x33, 0x44, 0x55, 0x66, 0, 0]
    );

    binding
        .write_config(
            bdf,
            ConfigOffset::new(capability_offset + 8).unwrap(),
            AccessWidth::Dword,
            0xfeed_beef,
        )
        .unwrap();
    let write = recording.writes.lock_irqsave().pop().unwrap();
    assert_eq!(write.0.value(), 0xfeed_beef);
    assert_eq!(write.1, DeviceId::new(7));
    assert_eq!(
        &write.0.capability_snapshot().bytes()[..8],
        &[0, 0, 0x33, 0x44, 0x55, 0x66, 0, 0]
    );

    // Effect results are not copied into root config storage: the next
    // read reaches the endpoint again and returns its fresh result.
    assert_eq!(
        binding
            .read_config(
                bdf,
                ConfigOffset::new(capability_offset + 8).unwrap(),
                AccessWidth::Dword,
            )
            .unwrap(),
        0x5a
    );
    let second_read = recording.reads.lock_irqsave().pop().unwrap();
    assert_eq!(second_read.0.effect(), PciConfigEffectId::new(7));
    assert_eq!(second_read.1, DeviceId::new(7));

    binding
        .write_config(
            bdf,
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            0x0406,
        )
        .unwrap();
    let command = recording.commands.lock_irqsave().pop().unwrap();
    assert!(command.0.memory_space_enable());
    assert!(command.0.bus_master_enable());
    assert!(command.0.interrupt_disable());
    assert_eq!(command.1, DeviceId::new(7));

    assert!(matches!(
        binding.read_config(
            bdf,
            ConfigOffset::new(capability_offset + 12).unwrap(),
            AccessWidth::Dword,
        ),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert!(recording.reads.lock_irqsave().is_empty());

    drop(lease);
    assert!(matches!(
        binding.read_config(
            bdf,
            ConfigOffset::new(capability_offset + 8).unwrap(),
            AccessWidth::Dword,
        ),
        Err(DeviceError::InvalidInput { .. })
    ));
}

#[test]
fn dynamic_interrupt_status_is_read_from_the_bound_endpoint() {
    let function_id = DeviceNodeId::new("intx-endpoint").unwrap();
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
    assert!(topology.function(&function_id).unwrap().intx().is_some());
    let root = Arc::new(PciRootState::new(Arc::clone(&topology)));
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        root,
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
        pending: true,
    });
    let mut grants = Vec::new();
    let lease = binding
        .bind_registered(
            &function_id,
            DeviceId::new(9),
            recording.clone(),
            &mut grants,
        )
        .unwrap();

    assert_eq!(
        binding
            .read_config(bdf, ConfigOffset::new(0x06).unwrap(), AccessWidth::Byte)
            .unwrap()
            & 0x08,
        0x08
    );
    drop(lease);
    // Teardown invokes endpoint-owned final IRQ withdrawal after the
    // binding admission has been closed and drained.
    assert_eq!(*recording.withdrawals.lock_irqsave(), 1);
    assert_eq!(
        binding
            .read_config(bdf, ConfigOffset::new(0x06).unwrap(), AccessWidth::Byte)
            .unwrap()
            & 0x08,
        0
    );
}
