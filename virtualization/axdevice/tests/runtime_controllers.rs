use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axdevice::{
    ControllerRegistration, DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DevicePlanRequest, DeviceRegistration, DeviceRequirements, DeviceRuntime,
    DeviceRuntimeBuilder, InterruptRegistrationError, ResourcePools, ResourceRequest, ResourceSlot,
    VmResourcePlan, VmResourcePlanner,
};
use axdevice_base::{
    BusAccess, BusResponse, ControllerInputId, Device, DeviceAccess, DeviceError,
    InterruptControllerId, InterruptSharing, InterruptTrigger, IrqError, IrqLine, IrqResult,
    Resource, VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

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
    lines: Arc<Mutex<Vec<IrqLine>>>,
}

impl DeviceFactory for IrqFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Dummy
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
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

fn irq_plan(
    controller: InterruptControllerId,
    devices: &[(&str, InterruptSharing)],
) -> VmResourcePlan {
    let mut pools = ResourcePools::new();
    pools
        .allow_fixed_controller_inputs(
            controller,
            ControllerInputId::new(32)..ControllerInputId::new(64),
        )
        .unwrap();
    let requests = devices.iter().map(|(id, sharing)| {
        DevicePlanRequest::new(
            *id,
            DeviceRequirements::new()
                .with_wired_irq(
                    irq_slot(),
                    controller,
                    InterruptTrigger::LevelTriggered,
                    *sharing,
                    ResourceRequest::Fixed(ControllerInputId::new(40)),
                )
                .unwrap(),
        )
        .unwrap()
    });
    VmResourcePlanner::new(pools).plan(requests).unwrap()
}

fn dummy_config(name: &str) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: name.into(),
        emu_type: EmulatedDeviceType::Dummy,
        ..Default::default()
    }
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
    let plan = irq_plan(id, &[("uart", InterruptSharing::Exclusive)]);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut factories = DeviceFactoryRegistry::new();
    factories
        .register(Arc::new(IrqFactory {
            slot: irq_slot(),
            lines,
        }))
        .unwrap();
    let mut builder = DeviceRuntimeBuilder::new(Default::default());

    assert!(
        builder
            .build_planned_device("uart", &dummy_config("uart"), &factories, &plan)
            .is_err()
    );
    let controller = Arc::new(TestController::new(id, Arc::default()));
    builder
        .register_bundle(controller_bundle(id, controller))
        .unwrap();
    builder
        .build_planned_device("uart", &dummy_config("uart"), &factories, &plan)
        .unwrap();
    let mut runtime = builder.finish(&plan).unwrap();
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
    let plan = irq_plan(
        id,
        &[
            ("left", InterruptSharing::Shared),
            ("right", InterruptSharing::Shared),
        ],
    );
    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut factories = DeviceFactoryRegistry::new();
    factories
        .register(Arc::new(IrqFactory {
            slot: irq_slot(),
            lines: lines.clone(),
        }))
        .unwrap();
    let mut builder = DeviceRuntimeBuilder::new(Default::default());
    builder
        .register_bundle(controller_bundle(id, controller))
        .unwrap();
    for device in ["right", "left"] {
        builder
            .build_planned_device(device, &dummy_config(device), &factories, &plan)
            .unwrap();
    }
    let _runtime = builder.finish(&plan).unwrap();

    let lines = lines.lock().unwrap();
    lines[0].assert().unwrap();
    assert_eq!(sink.last_level(), Some(true));
    lines[1].assert().unwrap();
    lines[0].deassert().unwrap();
    assert_eq!(sink.last_level(), Some(true));
    lines[1].deassert().unwrap();
    assert_eq!(sink.last_level(), Some(false));
}
