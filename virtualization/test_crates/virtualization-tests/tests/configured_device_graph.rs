use std::sync::{Arc, Mutex};

use axdevice::{
    AcpiDeviceSpec, AcpiNodeModel, ControllerRegistration, DeviceBuildContext, DeviceBundle,
    DeviceDeclaration, DeviceGraphBuilder, DeviceModel, DeviceNodeId, DeviceNodeSpec,
    DeviceRegistration, DeviceRequirements, FdtNodeModel, FdtNodeSpec, FirmwareBuildError,
    FirmwareModels, FirmwareProperty, ResourcePools, ResourceRequest, ResourceSlot,
    render_device_firmware,
};
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger, IrqResult,
    VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm::{
    ConfiguredDeviceCatalog, ConfiguredDeviceError, ConfiguredDeviceFactory,
    ConfiguredDeviceInstance, DeviceInstantiationContext, machine::MachineArchitecture,
};
use axvmconfig::{GuestConfig, VirtualDeviceRequest};

// Host tests link AxVM without a bare-metal linker script. These symbols only
// satisfy platform code that is unreachable from this graph-level test.
#[unsafe(no_mangle)]
pub extern "C" fn STACK_SIZE() {}

#[unsafe(no_mangle)]
pub static PAGE_SIZE: u8 = 0;

#[unsafe(no_mangle)]
pub static __PERCPU_TEMPLATE_ALIGN_START: u8 = 0;

#[unsafe(no_mangle)]
pub static __PERCPU_TEMPLATE_ALIGN_END: u8 = 0;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockLikeOptions {
    capacity: String,
}

struct BlockLikeFactory {
    built: Arc<Mutex<Vec<(u64, usize, String)>>>,
}

impl ConfiguredDeviceFactory for BlockLikeFactory {
    fn model_name(&self) -> &'static str {
        "virtio-blk-like"
    }

    fn instantiate(
        &self,
        request: &VirtualDeviceRequest,
        context: &DeviceInstantiationContext,
    ) -> Result<ConfiguredDeviceInstance, ConfiguredDeviceError> {
        let options = request
            .deserialize_options::<BlockLikeOptions>()
            .map_err(|error| ConfiguredDeviceError::InvalidOptions {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: error.to_string(),
            })?;
        let controller = context.default_wired_controller().ok_or_else(|| {
            ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            }
        })?;
        let model = Arc::new(BlockLikeModel {
            capacity: options.capacity,
            controller,
            built: self.built.clone(),
        });
        let firmware = FirmwareModels {
            fdt: Some(model.clone()),
            acpi: Some(model.clone()),
        };
        let dependency = context
            .default_wired_controller_node()
            .expect("the controller ID and graph dependency are one capability")
            .clone();
        Ok(ConfiguredDeviceInstance::new(model)
            .with_firmware(firmware)
            .with_dependency(dependency))
    }
}

struct BlockLikeModel {
    capacity: String,
    controller: InterruptControllerId,
    built: Arc<Mutex<Vec<(u64, usize, String)>>>,
}

impl DeviceModel for BlockLikeModel {
    fn declare(&self) -> axdevice::DeviceManagerResult<DeviceDeclaration> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new("registers")?,
                0x1000,
                0x1000,
                ResourceRequest::Auto,
            )?
            .with_wired_irq(
                ResourceSlot::new("irq")?,
                self.controller,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Auto,
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        let (base, _) = context.mmio(&ResourceSlot::new("registers")?)?;
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        self.built
            .lock()
            .unwrap()
            .push((base, irq.input().value(), self.capacity.clone()));
        Ok(DeviceBundle::new())
    }
}

impl FdtNodeModel for BlockLikeModel {
    fn render(
        &self,
        resources: &axdevice::ResolvedDeviceResources,
    ) -> Result<FdtNodeSpec, FirmwareBuildError> {
        let (base, _) = resources
            .mmio(&ResourceSlot::new("registers").unwrap())
            .map_err(firmware_error)?;
        let irq = resources
            .wired_irq(&ResourceSlot::new("irq").unwrap())
            .map_err(firmware_error)?;
        Ok(FdtNodeSpec {
            path: format!("/virtio@{base:x}"),
            properties: vec![FirmwareProperty {
                name: "interrupt".into(),
                value: (irq.input().value() as u64).to_be_bytes().to_vec(),
            }],
        })
    }
}

impl AcpiNodeModel for BlockLikeModel {
    fn render(
        &self,
        resources: &axdevice::ResolvedDeviceResources,
    ) -> Result<AcpiDeviceSpec, FirmwareBuildError> {
        let (base, _) = resources
            .mmio(&ResourceSlot::new("registers").unwrap())
            .map_err(firmware_error)?;
        Ok(AcpiDeviceSpec {
            path: format!("\\_SB.V{:03X}", (base >> 12) & 0xfff),
            aml: base.to_le_bytes().to_vec(),
        })
    }
}

fn firmware_error(error: axdevice::DeviceManagerError) -> FirmwareBuildError {
    FirmwareBuildError::InvalidModel {
        node: "virtio-blk-like".into(),
        detail: error.to_string(),
    }
}

struct TestController;
struct TestSink;

impl WiredIrqSink for TestSink {
    fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

impl VirtualInterruptController for TestController {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput> {
        Ok(WiredIrqInput::new(
            self.id(),
            input,
            trigger,
            Arc::new(TestSink),
        ))
    }
}

struct ControllerModel;

impl DeviceModel for ControllerModel {
    fn declare(&self) -> axdevice::DeviceManagerResult<DeviceDeclaration> {
        Ok(DeviceDeclaration::new())
    }

    fn build(
        &self,
        _context: &mut DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        let controller: Arc<dyn VirtualInterruptController> = Arc::new(TestController);
        Ok(DeviceBundle::from_registration(
            DeviceRegistration::InterruptController(ControllerRegistration::new(
                InterruptControllerId::new(0),
                controller,
            )),
        ))
    }
}

#[test]
fn configured_dyn_models_share_graph_firmware_and_runtime_resources() {
    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data1"
model = "virtio-blk-like"
capacity = "20GiB"

[[devices.virtual]]
id = "data0"
model = "virtio-blk-like"
capacity = "20GiB"
"#,
    )
    .unwrap();
    let built = Arc::new(Mutex::new(Vec::new()));
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog
        .register(Arc::new(BlockLikeFactory {
            built: built.clone(),
        }))
        .unwrap();

    let controller_id = DeviceNodeId::new("controller").unwrap();
    let context = DeviceInstantiationContext::new(MachineArchitecture::X86_64)
        .with_default_wired_controller(controller_id.clone(), InterruptControllerId::new(0));
    let mut graph = DeviceGraphBuilder::new();
    graph
        .add(DeviceNodeSpec::virtual_device(
            controller_id.clone(),
            Arc::new(ControllerModel),
        ))
        .unwrap();
    for request in &config.devices.virtual_devices {
        let node = catalog.instantiate_node(request, &context).unwrap();
        graph.add(node).unwrap();
    }

    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000_0000..0x1001_0000).unwrap();
    pools
        .add_auto_controller_inputs(
            InterruptControllerId::new(0),
            ControllerInputId::new(32)..ControllerInputId::new(40),
        )
        .unwrap();
    let graph = graph.declare().unwrap().resolve(pools).unwrap();

    let firmware = render_device_firmware(&graph).unwrap();

    let mut runtime = axdevice::DeviceRuntimeBuilder::new(Default::default());
    for node in graph.nodes() {
        runtime
            .build_graph_node(node, graph.resource_plan())
            .unwrap();
    }
    runtime.finish(graph.resource_plan()).unwrap();

    let mut built = built.lock().unwrap().clone();
    built.sort_by_key(|entry| entry.0);
    assert_eq!(
        built,
        vec![
            (0x1000_0000, 32, "20GiB".into()),
            (0x1000_1000, 33, "20GiB".into()),
        ]
    );
    assert_eq!(firmware.fdt().len(), 2);
    assert_eq!(firmware.acpi().len(), 2);
}

#[test]
fn configured_catalog_rejects_ambiguous_or_untyped_requests() {
    let built = Arc::new(Mutex::new(Vec::new()));
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog
        .register(Arc::new(BlockLikeFactory {
            built: built.clone(),
        }))
        .unwrap();
    assert!(matches!(
        catalog.register(Arc::new(BlockLikeFactory { built })),
        Err(ConfiguredDeviceError::DuplicateModel { .. })
    ));

    let context = DeviceInstantiationContext::new(MachineArchitecture::X86_64)
        .with_default_wired_controller(
            DeviceNodeId::new("controller").unwrap(),
            InterruptControllerId::new(0),
        );
    let unknown = VirtualDeviceRequest {
        id: "unknown0".into(),
        model: "not-registered".into(),
        options: toml::Table::new(),
    };
    assert!(matches!(
        catalog.instantiate_node(&unknown, &context),
        Err(ConfiguredDeviceError::UnknownVirtualDeviceModel { .. })
    ));

    let invalid = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data0"
model = "virtio-blk-like"
capacity = 20
"#,
    )
    .unwrap();
    assert!(matches!(
        catalog.instantiate_node(&invalid.devices.virtual_devices[0], &context),
        Err(ConfiguredDeviceError::InvalidOptions { .. })
    ));
}
