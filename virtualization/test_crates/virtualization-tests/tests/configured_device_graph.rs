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
        let registers = ResourceSlot::new("registers").unwrap();
        let interrupt = ResourceSlot::new("irq").unwrap();
        DeviceFirmwareSpec::interfaces(
            Some(vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("virtio")
                    .with_compatible("virtio,mmio")
                    .with_register(registers.clone())
                    .with_interrupt(interrupt.clone()),
            )]),
            Some(vec![AcpiContributionSpec::Conventional(
                AcpiDeviceSpec::new_indexed("VB", "LNRO0005")
                    .with_register(registers)
                    .with_interrupt(interrupt),
            )]),
        )
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

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
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
    catalog
        .register(module_path!(), BLOCK_LIKE_REGISTRATION)
        .unwrap();

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
        let [FdtContributionSpec::Conventional(fdt)] = firmware.fdt().unwrap() else {
            panic!("block-like model must expose one conventional FDT node");
        };
        let [AcpiContributionSpec::Conventional(acpi)] = firmware.acpi().unwrap() else {
            panic!("block-like model must expose one conventional ACPI node");
        };
        assert_eq!(fdt.node_name(), "virtio");
        assert_eq!(fdt.compatible(), ["virtio,mmio"]);
        assert_eq!(acpi.hid(), Some("LNRO0005"));
        let resources = graph.resources_for(node.id()).unwrap();
        assert!(resources.mmio(&fdt.register_slots()[0]).is_ok());
        assert!(resources.wired_irq(&fdt.interrupt_slots()[0]).is_ok());
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
fn ivc_channel_uses_catalog_and_planned_mmio_aperture() {
    const IVC_APERTURE_SIZE: u64 = 0x100_0000;

    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "ivc0"
model = "ivc-channel"
"#,
    )
    .unwrap();

    let mut catalog = ConfiguredDeviceCatalog::new();
    axvm::machine::register_devices(&mut catalog).unwrap();
    let controller_id = DeviceNodeId::new("controller").unwrap();
    let context = DeviceInstantiationContext::new()
        .with_default_wired_controller(controller_id.clone(), InterruptControllerId::new(0));
    let mut graph = DeviceGraphBuilder::new();
    graph
        .add(DeviceNodeSpec::virtual_device(
            controller_id,
            Arc::new(ControllerModel),
        ))
        .unwrap();
    for request in config.devices.virtual_device_requests() {
        let node = catalog.instantiate_node(request, &context).unwrap();
        graph.add(node).unwrap();
    }

    let mut pools = ResourcePools::new();
    pools
        .add_auto_mmio(0x1000_0000..0x1000_0000 + IVC_APERTURE_SIZE)
        .unwrap();
    pools
        .add_auto_controller_inputs(
            InterruptControllerId::new(0),
            ControllerInputId::new(32)..ControllerInputId::new(33),
        )
        .unwrap();
    let graph = graph.declare().unwrap().resolve(pools).unwrap();

    let ivc_id = DeviceNodeId::new("ivc0").unwrap();
    let registers = ResourceSlot::new("registers").unwrap();
    let notify = ResourceSlot::new("notify").unwrap();
    let resources = graph.resources_for(&ivc_id).unwrap();
    assert_eq!(
        resources.mmio(&registers).unwrap(),
        (0x1000_0000, IVC_APERTURE_SIZE)
    );
    assert_eq!(resources.wired_irq(&notify).unwrap().input().value(), 32);

    let ivc_node = graph
        .nodes()
        .find(|node| node.id() == &ivc_id)
        .expect("IVC node is present in the resolved graph");
    let [FdtContributionSpec::Conventional(fdt)] = ivc_node.firmware().fdt().unwrap() else {
        panic!("IVC model must expose one conventional FDT node");
    };
    assert_eq!(fdt.node_name(), "ivc-channel");
    assert_eq!(fdt.compatible(), ["axvisor,ivc-channel"]);
    assert_eq!(
        fdt.properties(),
        [
            DeviceFirmwareProperty::InterruptInput {
                name: "axvisor,notify-irq".into(),
                slot: notify.clone(),
            },
            DeviceFirmwareProperty::String {
                name: "status".into(),
                value: "okay".into(),
            },
            DeviceFirmwareProperty::U32 {
                name: "axvisor,ivc-version".into(),
                value: 1,
            },
        ]
    );
    assert_eq!(fdt.register_slots(), [registers]);
    assert_eq!(fdt.interrupt_slots(), [notify]);

    let mut runtime = axdevice::DeviceRuntimeBuilder::new(Default::default());
    for node in graph.nodes() {
        runtime
            .build_graph_node(node, graph.resource_plan())
            .unwrap();
    }
    let _runtime = runtime.finish(graph.resource_plan()).unwrap();
}

#[test]
fn ivc_channel_rejects_raw_notify_irq_option() {
    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "ivc0"
model = "ivc-channel"
notify_irq = 160
"#,
    )
    .unwrap();
    let request = config.devices.virtual_device_requests().first().unwrap();
    let controller_id = DeviceNodeId::new("controller").unwrap();
    let context = DeviceInstantiationContext::new()
        .with_default_wired_controller(controller_id, InterruptControllerId::new(0));

    let mut catalog = ConfiguredDeviceCatalog::new();
    axvm::machine::register_devices(&mut catalog).unwrap();
    assert!(matches!(
        catalog.instantiate_node(request, &context),
        Err(ConfiguredDeviceError::InvalidOptions { .. })
    ));
}

#[test]
fn axvm_catalog_owns_common_virtio_models() {
    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "disk0"
model = "virtio-blk"
backend = "ramdisk"
capacity = "2MiB"

[[devices.virtual]]
id = "net0"
model = "virtio-net"
guest_mac = [2, 0, 0, 0, 0, 1]
"#,
    )
    .unwrap();
    let context = DeviceInstantiationContext::new().with_default_wired_controller(
        DeviceNodeId::new("controller").unwrap(),
        InterruptControllerId::new(0),
    );
    let mut catalog = ConfiguredDeviceCatalog::new();
    axvm::machine::register_devices(&mut catalog).unwrap();

    assert!(
        catalog
            .instantiate_node(&config.devices.virtual_devices[0], &context)
            .is_ok()
    );
    assert!(matches!(
        catalog.instantiate_node(&config.devices.virtual_devices[1], &context),
        Err(ConfiguredDeviceError::Instantiation { .. })
    ));
}

#[test]
fn configured_catalog_rejects_ambiguous_or_untyped_requests() {
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog.register("first", BLOCK_LIKE_REGISTRATION).unwrap();
    assert!(matches!(
        catalog.register("second", BLOCK_LIKE_REGISTRATION),
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

#[test]
fn axvm_common_registration_rolls_back_the_complete_batch() {
    let conflicting_registration = ConfiguredModelRegistration {
        model: "ivc-channel",
        create: create_block_like,
    };
    let mut catalog = ConfiguredDeviceCatalog::new();
    catalog
        .register("preexisting-owner", conflicting_registration)
        .unwrap();

    assert!(matches!(
        axvm::machine::register_devices(&mut catalog),
        Err(ConfiguredDeviceError::DuplicateModel { .. })
    ));

    let serial = VirtualDeviceRequest {
        id: "serial0".into(),
        model: "pl011-mmio".into(),
        options: toml::Table::new(),
    };
    assert!(matches!(
        catalog.instantiate_node(&serial, &DeviceInstantiationContext::new()),
        Err(ConfiguredDeviceError::UnknownVirtualDeviceModel { .. })
    ));
}
