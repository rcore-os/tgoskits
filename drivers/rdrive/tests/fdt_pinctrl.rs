use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{vec, vec::Vec};

use fdt_edit::{Fdt, Node, NodeType, Property};
use rdif_pinctrl::{
    ConfigSetting, DriverGeneric, FdtPinctrlParser, FunctionId, GroupId,
    Interface as PinctrlInterface, MuxSetting, MuxValue, PinConfig, PinDesc, PinFunction, PinGroup,
    PinId, PinState, PinctrlDevice, PinctrlError,
};
use rdrive::{
    Platform, PlatformDevice, get_one,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static PINCTRL_APPLIED_BEFORE_PROBE: AtomicBool = AtomicBool::new(false);

struct TestPinctrl {
    calls: Vec<&'static str>,
    pins: Vec<PinDesc>,
    groups: Vec<PinGroup>,
    functions: Vec<PinFunction>,
}

impl TestPinctrl {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            pins: vec![PinDesc::new(PinId::new(1), None)],
            groups: vec![PinGroup::new(GroupId::new(1), None, vec![PinId::new(1)])],
            functions: vec![PinFunction::new(
                FunctionId::new(1),
                None,
                vec![GroupId::new(1)],
            )],
        }
    }
}

impl DriverGeneric for TestPinctrl {
    fn name(&self) -> &str {
        "test-pinctrl"
    }
}

impl PinctrlInterface for TestPinctrl {
    fn pins(&self) -> &[PinDesc] {
        &self.pins
    }

    fn groups(&self) -> &[PinGroup] {
        &self.groups
    }

    fn functions(&self) -> &[PinFunction] {
        &self.functions
    }

    fn apply_mux(&mut self, _setting: &MuxSetting) -> Result<(), PinctrlError> {
        self.calls.push("mux");
        Ok(())
    }

    fn apply_config(&mut self, _setting: &ConfigSetting) -> Result<(), PinctrlError> {
        self.calls.push("config");
        Ok(())
    }
}

struct TestParser;

impl FdtPinctrlParser for TestParser {
    fn parse_pinctrl_node(
        &self,
        _fdt: &Fdt,
        node: NodeType<'_>,
        state: &mut PinState,
    ) -> Result<(), PinctrlError> {
        let mut cells = node
            .as_node()
            .get_property("test,pins")
            .ok_or(PinctrlError::InvalidConfig)?
            .get_u32_iter();
        state.push_mux(MuxSetting::new(
            GroupId::new(cells.next().ok_or(PinctrlError::InvalidConfig)?),
            FunctionId::new(cells.next().ok_or(PinctrlError::InvalidConfig)?),
            MuxValue::new(cells.next().ok_or(PinctrlError::InvalidConfig)?),
        ));
        state.push_config(ConfigSetting::pin(
            PinId::new(1),
            PinConfig::DriveStrengthUa(4000),
        ));
        Ok(())
    }
}

struct FdtConsumerDevice;

impl DriverGeneric for FdtConsumerDevice {
    fn name(&self) -> &str {
        "fdt-consumer"
    }
}

fn register_test_pinctrl(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(PinctrlDevice::with_fdt_parser(
            TestPinctrl::new(),
            TestParser,
        ));
    Ok(())
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let pinctrl = get_one::<PinctrlDevice>()
        .ok_or_else(|| OnProbeError::other("pinctrl should be registered"))?;
    let pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock pinctrl: {err}")))?;
    PINCTRL_APPLIED_BEFORE_PROBE.store(
        pinctrl
            .typed_ref::<TestPinctrl>()
            .is_some_and(|pinctrl| pinctrl.calls == vec!["mux", "config"]),
        Ordering::SeqCst,
    );

    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(FdtConsumerDevice);
    Ok(())
}

static PINCTRL_REGISTER: DriverRegister = DriverRegister {
    name: "test pinctrl provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,pinctrl"],
        on_probe: register_test_pinctrl,
    }],
};

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test pinctrl consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_probe_applies_default_pinctrl_before_consumer_probe() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "pinctrl",
            &[
                prop_strs("compatible", &["test,pinctrl"]),
                prop_u32s("phandle", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "uart0-pins",
            &[
                prop_u32s("phandle", &[2]),
                prop_u32s("test,pins", &[1, 1, 7]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "consumer",
            &[
                prop_strs("compatible", &["test,consumer"]),
                prop_strs("pinctrl-names", &["default"]),
                prop_u32s("pinctrl-0", &[2]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(PINCTRL_REGISTER.clone());
    rdrive::register_add(CONSUMER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert!(PINCTRL_APPLIED_BEFORE_PROBE.load(Ordering::SeqCst));
    assert!(get_one::<FdtConsumerDevice>().is_some());
}

fn node_with_props(name: &str, props: &[Property]) -> Node {
    let mut node = Node::new(name);
    for prop in props {
        node.set_property(prop.clone());
    }
    node
}

fn prop_u32s(name: &str, values: &[u32]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(&value.to_be_bytes());
    }
    Property::new(name, data)
}

fn prop_strs(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}
