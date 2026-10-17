mod common;

use core::ptr::NonNull;
use std::{string::String, sync::Mutex, vec::Vec};

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static ENABLED_CLOCKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct ClockProvider;
struct ResourceConsumer;

impl DriverGeneric for ClockProvider {
    fn name(&self) -> &str {
        "optional-clock-provider"
    }
}

impl rdif_clk::Interface for ClockProvider {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), rdrive::KError> {
        ENABLED_CLOCKS
            .lock()
            .unwrap()
            .push(format!("enable:{}", id.raw()));
        Ok(())
    }

    fn get_rate(&self, _id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        Ok(50_000_000)
    }

    fn set_rate(&mut self, _id: rdif_clk::ClockId, _rate: u64) -> Result<(), rdrive::KError> {
        Ok(())
    }
}

impl DriverGeneric for ResourceConsumer {
    fn name(&self) -> &str {
        "optional-provider-resource-consumer"
    }
}

fn probe_clock_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_clk::Clk::new(ClockProvider));
    Ok(())
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe.info().prepare_resources(
        rdrive::probe::fdt::ResourcePrepareConfig::default().with_optional_clock_providers(),
    )?;
    probe.into_platform_device().register(ResourceConsumer);
    Ok(())
}

static CLOCK_REGISTER: DriverRegister = DriverRegister {
    name: "test optional clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,optional-clock-provider"],
        on_probe: probe_clock_provider,
    }],
};

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test optional provider resource consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,optional-provider-resource-consumer"],
        on_probe: probe_consumer,
    }],
};

/// A node that declares one rdrive-drivable clock and one firmware-owned clock, as the
/// RK3588 NPU does with its CRU gates and SCMI `clk_npu`, prepares the drivable clock
/// and probes; the skipped clock is left to the firmware.
#[test]
fn fdt_resource_prepare_skips_clock_without_rdif_provider_when_optional() {
    ENABLED_CLOCKS.lock().unwrap().clear();

    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller",
            &[
                prop_strs("compatible", &["test,optional-clock-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    // Firmware-owned clock: the node exists, but no driver registers an rdif-clk
    // interface for it, mirroring an SCMI clock protocol without its driver.
    fdt.add_node(
        root,
        node_with_props(
            "firmware-clock",
            &[
                prop_strs("compatible", &["test,firmware-owned-clock-provider"]),
                prop_u32s("phandle", &[2]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "npu@fdab0000",
            &[
                prop_strs("compatible", &["test,optional-provider-resource-consumer"]),
                prop_u32s("clocks", &[1, 4, 2, 6]),
                prop_strs("clock-names", &["aclk0", "clk_npu"]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(CLOCK_REGISTER.clone());
    rdrive::register_add(CONSUMER_REGISTER.clone());

    probe_all(true).expect("clock without an rdrive provider must not fail the probe");

    assert_eq!(
        *ENABLED_CLOCKS.lock().unwrap(),
        vec![String::from("enable:4")]
    );
    assert!(rdrive::get_one::<ResourceConsumer>().is_some());
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
