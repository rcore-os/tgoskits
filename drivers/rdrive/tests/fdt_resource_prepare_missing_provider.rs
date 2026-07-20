use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct ResourceConsumer;

impl DriverGeneric for ResourceConsumer {
    fn name(&self) -> &str {
        "resource-consumer"
    }
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .info()
        .prepare_resources(rdrive::probe::fdt::ResourcePrepareConfig::default())?;
    probe.into_platform_device().register(ResourceConsumer);
    Ok(())
}

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test missing provider resource consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,missing-provider-resource-consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_resource_prepare_rejects_declared_clock_without_rdif_provider() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller",
            &[prop_u32s("phandle", &[1]), prop_u32s("#clock-cells", &[1])],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "mmc@16020000",
            &[
                prop_strs("compatible", &["test,missing-provider-resource-consumer"]),
                prop_u32s("clocks", &[1, 4]),
                prop_strs("clock-names", &["ciu"]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(CONSUMER_REGISTER.clone());

    let err = probe_all(true).expect_err("declared clock without rdif-clk should fail probe");

    assert!(
        err.to_string().contains("has no rdif-clk interface"),
        "unexpected error: {err}"
    );
    assert!(rdrive::get_one::<ResourceConsumer>().is_none());
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
