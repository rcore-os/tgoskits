use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform, get_one,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

/// A clock provider that rejects every `set_rate`, modelling a rate the CRU
/// doesn't implement (e.g. a VOP root clock).
struct RejectingClockProvider;

impl DriverGeneric for RejectingClockProvider {
    fn name(&self) -> &str {
        "rejecting-clock"
    }
}

impl rdif_clk::Interface for RejectingClockProvider {
    fn perper_enable(&mut self) {}

    fn get_rate(&self, _id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        Ok(0)
    }

    fn set_rate(&mut self, _id: rdif_clk::ClockId, _rate: u64) -> Result<(), rdrive::KError> {
        Err(rdrive::KError::Unknown("clock rate not supported"))
    }
}

fn probe_clock(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_clk::Clk::new(RejectingClockProvider));
    Ok(())
}

static CLOCK_REGISTER: DriverRegister = DriverRegister {
    name: "rejecting clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,rejecting-clock"],
        on_probe: probe_clock,
    }],
};

/// The consumer whose node carries `assigned-clock-rates` the provider rejects.
struct ConsumerDevice;

impl DriverGeneric for ConsumerDevice {
    fn name(&self) -> &str {
        "assigned-clock-consumer"
    }
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe.into_platform_device().register(ConsumerDevice);
    Ok(())
}

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "assigned-clock consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,best-effort-consumer"],
        on_probe: probe_consumer,
    }],
};

/// `assigned-clocks`/`assigned-clock-rates` are best-effort: a rate the clock
/// provider can't set must log and continue, not abort the consumer's probe.
#[test]
fn assigned_clocks_are_best_effort_and_do_not_abort_probe() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller@1000",
            &[
                prop_strs("compatible", &["test,rejecting-clock"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "vop@2000",
            &[
                prop_strs("compatible", &["test,best-effort-consumer"]),
                prop_u32s("assigned-clocks", &[1, 0]),
                prop_u32s("assigned-clock-rates", &[100_000_000]),
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

    // The unsupported assigned-clock rate must not abort probing.
    probe_all(true).expect("FDT probe should succeed despite a rejected assigned-clock rate");
    assert!(
        get_one::<ConsumerDevice>().is_some(),
        "consumer must still register when its assigned-clock rate is unsupported"
    );
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
