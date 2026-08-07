use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axdevice::*;
use axdevice_base::*;

#[derive(Default)]
struct RecordingSink {
    levels: Mutex<Vec<bool>>,
}

impl RecordingSink {
    fn last_level(&self) -> Option<bool> {
        self.levels.lock().unwrap().last().copied()
    }
}

impl WiredIrqSink for RecordingSink {
    fn set_level(&self, _input: ControllerInputId, asserted: bool) -> IrqResult {
        self.levels.lock().unwrap().push(asserted);
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

struct TestController {
    id: InterruptControllerId,
    sink: Arc<RecordingSink>,
    inputs: Mutex<BTreeMap<ControllerInputId, WiredIrqInput>>,
}

impl TestController {
    fn new(id: InterruptControllerId, sink: Arc<RecordingSink>) -> Self {
        Self {
            id,
            sink,
            inputs: Mutex::new(BTreeMap::new()),
        }
    }
}

impl VirtualInterruptController for TestController {
    fn id(&self) -> InterruptControllerId {
        self.id
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput> {
        let mut inputs = self.inputs.lock().unwrap();
        if let Some(existing) = inputs.get(&input) {
            if existing.trigger() != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: axdevice_base::InterruptEndpoint::Wired {
                        controller: self.id,
                        input,
                    },
                    operation: "open test controller input",
                    detail: "trigger mismatch".into(),
                });
            }
            return Ok(existing.clone());
        }
        let created = WiredIrqInput::new(self.id, input, trigger, self.sink.clone());
        inputs.insert(input, created.clone());
        Ok(created)
    }
}

struct LineDevice {
    _line: IrqLine,
}

impl Device for LineDevice {
    fn name(&self) -> &str {
        "planned-line-device"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn access(
        &self,
        _access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        Err(DeviceError::NotFound)
    }
}

struct IrqFactory {
    slot: ResourceSlot,
    controller: InterruptControllerId,
    sharing: InterruptSharing,
    lines: Arc<Mutex<Vec<IrqLine>>>,
    probe_wrong_accessor: bool,
}

impl DeviceModel for IrqFactory {
    fn requirements(&self) -> axdevice::DeviceManagerResult<DeviceRequirements> {
        let requirements = DeviceRequirements::new().with_wired_irq(
            self.slot.clone(),
            self.controller,
            InterruptTrigger::LevelTriggered,
            self.sharing,
            ResourceRequest::Fixed(ControllerInputId::new(40)),
        )?;
        Ok(requirements)
    }

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        if self.probe_wrong_accessor {
            assert!(context.mmio(&self.slot).is_err());
        }
        let line = context.irq(&self.slot)?;
        self.lines.lock().unwrap().push(line.clone());
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(LineDevice { _line: line }),
        )))
    }
}

struct MmioDevice {
    resource: [Resource; 1],
}

impl MmioDevice {
    fn new(base: u64) -> Self {
        Self {
            resource: [Resource::MmioRange { base, size: 0x1000 }],
        }
    }
}

impl Device for MmioDevice {
    fn name(&self) -> &str {
        "test-mmio"
    }

    fn resources(&self) -> &[Resource] {
        &self.resource
    }

    fn access(
        &self,
        _access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        Err(DeviceError::NotFound)
    }
}

fn irq_slot() -> ResourceSlot {
    ResourceSlot::new("irq").unwrap()
}

fn irq_graph(
    controller: InterruptControllerId,
    devices: &[&str],
    factory: Arc<dyn DeviceModel>,
) -> ResolvedDeviceGraph {
    let mut pools = ResourcePools::new();
    pools
        .allow_fixed_controller_inputs(
            controller,
            ControllerInputId::new(32)..ControllerInputId::new(64),
        )
        .unwrap();
    let mut graph = DeviceGraphBuilder::new();
    for id in devices {
        graph
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new(*id).unwrap(),
                factory.clone(),
            ))
            .unwrap();
    }
    graph.declare().unwrap().resolve(pools).unwrap()
}

fn controller_bundle(
    id: InterruptControllerId,
    controller: Arc<dyn VirtualInterruptController>,
) -> DeviceBundle {
    DeviceBundle::from_registration(DeviceRegistration::InterruptController(
        ControllerRegistration::new(id, controller),
    ))
}

#[test]
fn controller_registration_is_validated_and_atomic() {
    let registered = InterruptControllerId::new(1);
    let reported = InterruptControllerId::new(2);
    let controller = Arc::new(TestController::new(reported, Arc::default()));
    let mut runtime = DeviceRuntime::default();
    let error = runtime
        .register_bundle(controller_bundle(registered, controller))
        .unwrap_err();
    assert!(matches!(
        error,
        DeviceManagerError::InterruptRegistration(
            InterruptRegistrationError::ControllerIdMismatch { .. }
        )
    ));

    let controller = Arc::new(TestController::new(registered, Arc::default()));
    runtime
        .register_bundle(controller_bundle(registered, controller.clone()))
        .unwrap();
    assert!(matches!(
        runtime.register_bundle(controller_bundle(registered, controller)),
        Err(DeviceManagerError::InterruptRegistration(
            InterruptRegistrationError::DuplicateController { .. }
        ))
    ));

    let id = InterruptControllerId::new(3);
    let controller = Arc::new(TestController::new(id, Arc::default()));
    let mut runtime = DeviceRuntime::default();
    runtime
        .register_bundle(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(MmioDevice::new(0x1000)),
        )))
        .unwrap();

    let bundle = controller_bundle(id, controller).with_registration(DeviceRegistration::Device(
        Arc::new(MmioDevice::new(0x1000)),
    ));
    assert!(runtime.register_bundle(bundle).is_err());
    assert!(runtime.interrupt_controller(id).is_err());
}

#[test]
fn failed_build_releases_claims_for_retry() {
    let id = InterruptControllerId::new(4);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(IrqFactory {
        slot: irq_slot(),
        controller: id,
        sharing: InterruptSharing::Exclusive,
        lines,
        probe_wrong_accessor: true,
    });
    let graph = irq_graph(id, &["uart"], factory);
    let node = graph.nodes().next().unwrap();
    let mut builder = DeviceRuntimeBuilder::new(Default::default());

    assert!(
        builder
            .build_graph_node(node, graph.resource_plan())
            .is_err()
    );
    let controller = Arc::new(TestController::new(id, Arc::default()));
    builder
        .register_bundle(controller_bundle(id, controller))
        .unwrap();
    builder
        .build_graph_node(node, graph.resource_plan())
        .unwrap();
    let mut runtime = builder.finish(graph.resource_plan()).unwrap();
    let late_id = InterruptControllerId::new(7);
    let late = Arc::new(TestController::new(late_id, Arc::default()));
    assert!(matches!(
        runtime.register_bundle(controller_bundle(late_id, late)),
        Err(DeviceManagerError::InvalidState { .. })
    ));
}

#[test]
fn shared_level_endpoints_preserve_wired_or() {
    let id = InterruptControllerId::new(5);
    let sink = Arc::new(RecordingSink::default());
    let controller = Arc::new(TestController::new(id, sink.clone()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(IrqFactory {
        slot: irq_slot(),
        controller: id,
        sharing: InterruptSharing::Shared,
        lines: lines.clone(),
        probe_wrong_accessor: false,
    });
    let graph = irq_graph(id, &["left", "right"], factory);
    let mut builder = DeviceRuntimeBuilder::new(Default::default());
    builder
        .register_bundle(controller_bundle(id, controller))
        .unwrap();
    for node in graph.nodes() {
        builder
            .build_graph_node(node, graph.resource_plan())
            .unwrap();
    }
    let _runtime = builder.finish(graph.resource_plan()).unwrap();

    let lines = lines.lock().unwrap();
    lines[0].assert().unwrap();
    assert_eq!(sink.last_level(), Some(true));
    lines[1].assert().unwrap();
    lines[0].deassert().unwrap();
    assert_eq!(sink.last_level(), Some(true));
    lines[1].deassert().unwrap();
    assert_eq!(sink.last_level(), Some(false));
}
