use std::sync::{Arc, Mutex};

use axdevice::*;
use axdevice_base::*;
use axvm::*;
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

static BUILT: Mutex<Vec<(u64, usize, String)>> = Mutex::new(Vec::new());

const BLOCK_LIKE_REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-blk-like",
    create: create_block_like,
};

fn create_block_like(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = request
        .deserialize_options::<BlockLikeOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            })?;
    let dependency = context
        .default_wired_controller_node()
        .expect("the controller ID and graph dependency are one capability")
        .clone();
    Ok(DeviceNodeSpec::virtual_device(
        id,
        Arc::new(BlockLikeModel {
            capacity: options.capacity,
            controller,
        }),
    )
    .with_dependency(dependency))
}

struct BlockLikeModel {
    capacity: String,
    controller: InterruptControllerId,
}

impl DeviceModel for BlockLikeModel {
    fn requirements(&self) -> axdevice::DeviceManagerResult<DeviceRequirements> {
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
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::new("virtio")
            .with_compatible("virtio,mmio")
            .with_acpi_hid("LNRO0005")
            .with_register(ResourceSlot::new("registers").unwrap())
            .with_interrupt(ResourceSlot::new("irq").unwrap())
    }

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> axdevice::DeviceManagerResult<DeviceBundle> {
        let (base, _) = context.mmio(&ResourceSlot::new("registers")?)?;
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        BUILT
            .lock()
            .unwrap()
            .push((base, irq.input().value(), self.capacity.clone()));
        Ok(DeviceBundle::new())
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
    fn requirements(&self) -> axdevice::DeviceManagerResult<DeviceRequirements> {
        Ok(DeviceRequirements::new())
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
fn configured_dyn_models_share_graph_metadata_and_runtime_resources() {
    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data1"
model = "virtio-blk-like"
capacity = "40GiB"

[[devices.virtual]]
id = "data0"
model = "virtio-blk-like"
capacity = "20GiB"
"#,
    )
    .unwrap();
    BUILT.lock().unwrap().clear();
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog.register(BLOCK_LIKE_REGISTRATION).unwrap();

    let controller_id = DeviceNodeId::new("controller").unwrap();
    let context = DeviceInstantiationContext::new()
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

    for node in graph
        .nodes()
        .filter(|node| node.id().as_str().starts_with("data"))
    {
        let firmware = node.firmware();
        assert_eq!(firmware.node_name().map(String::as_str), Some("virtio"));
        assert_eq!(firmware.compatible(), ["virtio,mmio"]);
        assert_eq!(firmware.acpi_hid().map(String::as_str), Some("LNRO0005"));
        let resources = graph.resources_for(node.id()).unwrap();
        assert!(resources.mmio(&firmware.register_slots()[0]).is_ok());
        assert!(resources.wired_irq(&firmware.interrupt_slots()[0]).is_ok());
    }

    let mut runtime = axdevice::DeviceRuntimeBuilder::new(Default::default());
    for node in graph.nodes() {
        runtime
            .build_graph_node(node, graph.resource_plan())
            .unwrap();
    }
    runtime.finish(graph.resource_plan()).unwrap();

    let mut built = BUILT.lock().unwrap().clone();
    built.sort_by_key(|entry| entry.0);
    assert_eq!(
        built,
        vec![
            (0x1000_0000, 32, "20GiB".into()),
            (0x1000_1000, 33, "40GiB".into()),
        ]
    );
}

#[test]
fn configured_catalog_rejects_ambiguous_or_untyped_requests() {
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog.register(BLOCK_LIKE_REGISTRATION).unwrap();
    assert!(matches!(
        catalog.register(BLOCK_LIKE_REGISTRATION),
        Err(ConfiguredDeviceError::DuplicateModel { .. })
    ));

    let context = DeviceInstantiationContext::new().with_default_wired_controller(
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

    let unknown_option = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data0"
model = "virtio-blk-like"
capacity = "20GiB"
cache = "writeback"
"#,
    )
    .unwrap();
    assert!(matches!(
        catalog.instantiate_node(&unknown_option.devices.virtual_devices[0], &context),
        Err(ConfiguredDeviceError::InvalidOptions { .. })
    ));
}
