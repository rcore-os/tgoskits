use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axdevice::{
    ControllerInputId, ControllerRegistration, ControllerRole, DeviceBuildContext, DeviceBundle,
    DeviceModelId, DeviceRegistration, DeviceRequirements, InterruptControllerId,
    InterruptSourceKind, InterruptTopology, ResolvedDeviceResources, ResourceSlot,
    VirtualDeviceModel, VirtualDeviceModelRegistry, WiredInterruptInputs,
};
use axdevice_base::{
    BusAccess, BusResponse, Device, DeviceError, InterruptTriggerMode, IrqLine, IrqResult,
    Resource, WiredIrqInput, WiredIrqSink,
};
use axvm_types::InterruptDelivery;

#[test]
fn model_build_resolves_named_irq_without_exposing_topology_ids() {
    let level = Arc::new(AtomicBool::new(false));
    let topology = InterruptTopology::new(InterruptDelivery::Mediated);
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
        )
        .unwrap();
    let mut registry = VirtualDeviceModelRegistry::new();
    registry.register(Arc::new(TestModel)).unwrap();
    let context = DeviceBuildContext::new(&topology, &resources);

    let _bundle = registry
        .build(
            &DeviceModelId::new("test-uart").unwrap(),
            &resources,
            &context,
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
                InterruptSourceKind::Software,
            )
    }

    fn build(
        &self,
        _resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        irq.raise()?;
        Ok(DeviceRegistration::Device(Arc::new(LineOwningDevice(irq))).into())
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

struct LineOwningDevice(IrqLine);

impl Device for LineOwningDevice {
    fn name(&self) -> &str {
        "line-owner"
    }

    fn resources(&self) -> &[Resource] {
        &[]
    }

    fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let _keep_line_alive = &self.0;
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
