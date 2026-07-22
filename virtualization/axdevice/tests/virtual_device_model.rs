use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axdevice::{
    AxVmDevices, ControllerInputId, ControllerRegistration, ControllerRole, DeviceBuildContext,
    DeviceBundle, DeviceManagerError, DeviceManagerResult, DeviceModelId, DeviceRegistration,
    DeviceRequirements, InterruptControllerId, InterruptTopology, PlannedIrqConnection,
    ResolvedDeviceResources, ResourceSlot, VirtualDeviceModel, VirtualDeviceModelRegistry,
    WiredInterruptInputs, WiredIrqClaim,
};
use axdevice_base::{
    BusAccess, BusResponse, Device, DeviceError, InterruptSharing, InterruptTriggerMode,
    InvalidResourceReason, IrqLine, IrqResult, RegistryError, Resource, WiredIrqInput,
    WiredIrqSink,
};

#[test]
fn model_build_resolves_named_irq_without_exposing_topology_ids() {
    let level = Arc::new(AtomicBool::new(false));
    let (topology, authority) = InterruptTopology::new();
    topology
        .register_controller(
            ControllerRegistration::new(InterruptControllerId::new(0), ControllerRole::Default)
                .with_wired_inputs(Arc::new(TestInputs(level.clone()))),
        )
        .unwrap();
    let resources = ResolvedDeviceResources::new()
        .with_mmio(ResourceSlot::new("registers").unwrap(), 0x0900_0000, 0x1000)
        .unwrap()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            ControllerInputId::new(33),
            InterruptTriggerMode::LevelTriggered,
            InterruptSharing::Exclusive,
        )
        .unwrap();
    let mut registry = VirtualDeviceModelRegistry::new();
    registry.register(Arc::new(TestModel)).unwrap();
    let context = DeviceBuildContext::new(&topology, &authority, &resources);

    let _bundle = registry
        .build(
            &DeviceModelId::new("test-uart").unwrap(),
            &resources,
            context,
        )
        .unwrap();

    assert!(level.load(Ordering::Acquire));
}

#[test]
fn named_pio_resources_are_distinct_from_mmio_resources() {
    let slot = ResourceSlot::new("registers").unwrap();
    let requirements = DeviceRequirements::new()
        .with_pio(slot.clone(), 8, 8)
        .unwrap();
    assert!(matches!(
        &requirements.entries()[0],
        axdevice::DeviceRequirement::Pio {
            size: 8,
            alignment: 8,
            ..
        }
    ));

    let resources = ResolvedDeviceResources::new()
        .with_pio(slot.clone(), 0x3f8, 8)
        .unwrap();
    assert_eq!(resources.pio(&slot).unwrap(), (0x3f8, 8));
    assert!(resources.mmio(&slot).is_err());
}

#[test]
fn pio_resource_may_end_at_the_architectural_limit() {
    let registers = ResourceSlot::new("registers").unwrap();

    let resources = ResolvedDeviceResources::new()
        .with_pio(registers.clone(), 0xfff8, 8)
        .unwrap();

    assert_eq!(resources.pio(&registers).unwrap(), (0xfff8, 8));
}

#[test]
fn duplicate_model_registration_preserves_the_original_model() {
    let id = DeviceModelId::new("test-uart").unwrap();
    let mut registry = VirtualDeviceModelRegistry::new();
    registry.register(Arc::new(TestModel)).unwrap();

    assert!(registry.register(Arc::new(DuplicateTestModel)).is_err());

    let requirements = registry.requirements(&id, None).unwrap();
    assert!(matches!(
        requirements.entries().first(),
        Some(axdevice::DeviceRequirement::Mmio { size: 0x1000, .. })
    ));
}

#[test]
fn exclusive_planned_irq_cannot_be_registered_by_two_devices() {
    let level = Arc::new(AtomicBool::new(false));
    let (topology, authority) = InterruptTopology::new();
    topology
        .register_controller(
            ControllerRegistration::new(InterruptControllerId::new(0), ControllerRole::Default)
                .with_wired_inputs(Arc::new(TestInputs(level))),
        )
        .unwrap();
    let resources = ResolvedDeviceResources::new()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            ControllerInputId::new(33),
            InterruptTriggerMode::LevelTriggered,
            InterruptSharing::Exclusive,
        )
        .unwrap();
    let mut models = VirtualDeviceModelRegistry::new();
    models.register(Arc::new(TestModel)).unwrap();
    let model = DeviceModelId::new("test-uart").unwrap();
    let first_context = DeviceBuildContext::new(&topology, &authority, &resources);
    let first = models.build(&model, &resources, first_context).unwrap();
    let mut devices = AxVmDevices::empty();
    devices
        .register_bundle_with_topology(first, &topology)
        .unwrap();
    assert!(matches!(
        devices.interrupt_endpoint_resources().next(),
        Some((_, Resource::WiredIrq {
            controller,
            input,
            sharing: InterruptSharing::Exclusive,
            ..
        })) if *controller == InterruptControllerId::new(0)
            && *input == ControllerInputId::new(33)
    ));

    let second_context = DeviceBuildContext::new(&topology, &authority, &resources);
    let second = models.build(&model, &resources, second_context);
    assert!(matches!(
        second,
        Err(DeviceManagerError::ResourceConflict { .. })
    ));
}

#[test]
fn failed_bundle_registration_releases_its_planned_irq_claim() {
    let (topology, authority) = InterruptTopology::new();
    topology
        .register_controller(
            ControllerRegistration::new(InterruptControllerId::new(0), ControllerRole::Default)
                .with_wired_inputs(Arc::new(TestInputs(Arc::new(AtomicBool::new(false))))),
        )
        .unwrap();
    let resources = ResolvedDeviceResources::new()
        .with_mmio(ResourceSlot::new("registers").unwrap(), 0x1000, 0x1000)
        .unwrap()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            ControllerInputId::new(33),
            InterruptTriggerMode::LevelTriggered,
            InterruptSharing::Exclusive,
        )
        .unwrap();
    let mut models = VirtualDeviceModelRegistry::new();
    models.register(Arc::new(TestModel)).unwrap();
    let model = DeviceModelId::new("test-uart").unwrap();
    let mut devices = AxVmDevices::empty();
    devices
        .register_bundle(DeviceRegistration::Device(Arc::new(MmioDevice::new(0x1000))).into())
        .unwrap();

    let failed_context = DeviceBuildContext::new(&topology, &authority, &resources);
    let failed = models.build(&model, &resources, failed_context).unwrap();
    assert!(matches!(
        devices.register_bundle_with_topology(failed, &topology),
        Err(DeviceManagerError::Registry(
            RegistryError::AddressConflict { .. }
        ))
    ));
    assert!(topology.active_endpoint_resources().is_empty());

    let retry_resources = ResolvedDeviceResources::new()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            ControllerInputId::new(33),
            InterruptTriggerMode::LevelTriggered,
            InterruptSharing::Exclusive,
        )
        .unwrap();
    let retry_context = DeviceBuildContext::new(&topology, &authority, &retry_resources);
    let retry = models
        .build(&model, &retry_resources, retry_context)
        .unwrap();
    devices
        .register_bundle_with_topology(retry, &topology)
        .unwrap();
    assert_eq!(devices.interrupt_endpoint_resources().count(), 1);
    assert_eq!(topology.active_endpoint_resources().len(), 1);
}

#[test]
fn device_cannot_declare_an_interrupt_endpoint_without_a_planner_claim() {
    let mut devices = AxVmDevices::empty();
    let resource = Resource::WiredIrq {
        controller: InterruptControllerId::new(0),
        input: ControllerInputId::new(33),
        trigger: InterruptTriggerMode::LevelTriggered,
        sharing: InterruptSharing::Exclusive,
    };

    assert!(matches!(
        devices.register_bundle(
            DeviceRegistration::Device(Arc::new(DeclaredIrqDevice([resource]))).into()
        ),
        Err(DeviceManagerError::Registry(
            RegistryError::InvalidResource {
                reason: InvalidResourceReason::UnbackedInterruptEndpoint,
                ..
            }
        ))
    ));
}

#[test]
fn topology_connection_requires_a_planner_claim() {
    let _connect: fn(
        &InterruptTopology,
        WiredIrqClaim,
    ) -> DeviceManagerResult<PlannedIrqConnection> = InterruptTopology::connect_irq;
}

struct TestModel;

impl VirtualDeviceModel for TestModel {
    fn model_id(&self) -> DeviceModelId {
        DeviceModelId::new("test-uart").unwrap()
    }

    fn requirements(
        &self,
        _template: Option<&axdevice::DeviceTemplate>,
    ) -> axdevice::DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(ResourceSlot::new("registers")?, 0x1000, 0x1000)?
            .with_wired_irq(
                ResourceSlot::new("irq")?,
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            )
    }

    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        irq.raise()?;
        let device_resources = resources
            .mmio(&ResourceSlot::new("registers")?)
            .map(|(base, size)| vec![Resource::MmioRange { base, size }])
            .unwrap_or_default();
        Ok(DeviceRegistration::Device(Arc::new(LineOwningDevice {
            irq,
            resources: device_resources,
        }))
        .into())
    }
}

struct DuplicateTestModel;

impl VirtualDeviceModel for DuplicateTestModel {
    fn model_id(&self) -> DeviceModelId {
        DeviceModelId::new("test-uart").unwrap()
    }

    fn requirements(
        &self,
        _template: Option<&axdevice::DeviceTemplate>,
    ) -> axdevice::DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(ResourceSlot::new("registers")?, 0x2000, 0x1000)
    }

    fn build(
        &self,
        _resources: &ResolvedDeviceResources,
        _context: &DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        unreachable!("duplicate model must not replace the registered model")
    }
}

struct LineOwningDevice {
    irq: IrqLine,
    resources: Vec<Resource>,
}

impl Device for LineOwningDevice {
    fn name(&self) -> &str {
        "line-owner"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let _keep_line_alive = &self.irq;
        Err(DeviceError::NotFound)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct DeclaredIrqDevice([Resource; 1]);

impl Device for DeclaredIrqDevice {
    fn name(&self) -> &str {
        "declared-irq-device"
    }

    fn resources(&self) -> &[Resource] {
        &self.0
    }

    fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
        Err(DeviceError::NotFound)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MmioDevice([Resource; 1]);

impl MmioDevice {
    fn new(base: u64) -> Self {
        Self([Resource::MmioRange { base, size: 0x1000 }])
    }
}

impl Device for MmioDevice {
    fn name(&self) -> &str {
        "mmio-device"
    }

    fn resources(&self) -> &[Resource] {
        &self.0
    }

    fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
        Err(DeviceError::NotFound)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TestInputs(Arc<AtomicBool>);

impl WiredInterruptInputs for TestInputs {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        Ok(WiredIrqInput::new(
            InterruptControllerId::new(0),
            input,
            trigger,
            Arc::new(TestSink(self.0.clone())),
        ))
    }
}

struct TestSink(Arc<AtomicBool>);

impl WiredIrqSink for TestSink {
    fn set_level(&self, _input: ControllerInputId, asserted: bool) -> IrqResult {
        self.0.store(asserted, Ordering::Release);
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}
