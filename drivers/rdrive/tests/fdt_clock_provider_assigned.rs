use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct ClockProviderDevice;

impl DriverGeneric for ClockProviderDevice {
    fn name(&self) -> &str {
        "clock-provider"
    }
}

impl rdif_clk::Interface for ClockProviderDevice {
    fn perper_enable(&mut self) {}

    fn get_rate(&self, _id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        Ok(0)
    }

    fn set_rate(&mut self, _id: rdif_clk::ClockId, _rate: u64) -> Result<(), rdrive::KError> {
        Ok(())
    }
}

fn probe_clock_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_clk::Clk::new(ClockProviderDevice));
    Ok(())
}

static CLOCK_PROVIDER_REGISTER: DriverRegister = DriverRegister {
    name: "test clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,clock-provider"],
        on_probe: probe_clock_provider,
    }],
};

#[test]
fn fdt_probe_does_not_apply_assigned_clocks_before_clock_provider_registers() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "late-parent-clock",
            &[prop_u32s("phandle", &[2]), prop_u32s("#clock-cells", &[1])],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller@1000",
            &[
                prop_strs("compatible", &["test,clock-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#clock-cells", &[1]),
                prop_u32s("assigned-clocks", &[2, 9]),
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
    rdrive::register_add(CLOCK_PROVIDER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert!(rdrive::get_one::<rdif_clk::Clk>().is_some());
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
