use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, NodeType, Property};
use rdif_pinctrl::{
    DriverGeneric, FdtPinctrlParser, Interface as PinctrlInterface, PinState, PinctrlDevice,
    PinctrlError,
};
use rdrive::{
    Platform, PlatformDevice,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct TestPinctrl;

impl DriverGeneric for TestPinctrl {
    fn name(&self) -> &str {
        "test-pinctrl"
    }
}

impl PinctrlInterface for TestPinctrl {}

struct TestParser;

impl FdtPinctrlParser for TestParser {
    fn parse_pinctrl_node(
        &self,
        _fdt: &Fdt,
        _node: NodeType<'_>,
        _state: &mut PinState,
    ) -> Result<(), PinctrlError> {
        Err(PinctrlError::InvalidConfig)
    }
}

struct ShouldNotProbe;

impl DriverGeneric for ShouldNotProbe {
    fn name(&self) -> &str {
        "should-not-probe"
    }
}

fn register_test_pinctrl(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(PinctrlDevice::with_fdt_parser(TestPinctrl, TestParser));
    Ok(())
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(ShouldNotProbe);
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
    name: "bad pinctrl consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_probe_reports_bad_default_pinctrl_before_consumer_probe() {
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
            "consumer",
            &[
                prop_strs("compatible", &["test,consumer"]),
                prop_strs("pinctrl-names", &["default"]),
                prop_u32s("pinctrl-0", &[99]),
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

    let err = probe_all(true).expect_err("bad pinctrl phandle should abort consumer probe");

    assert!(format!("{err}").contains("failed to apply default pinctrl"));
    assert!(rdrive::get_one::<ShouldNotProbe>().is_none());
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
