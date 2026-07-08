use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct PowerProvider;
struct ResourceConsumer;

impl DriverGeneric for PowerProvider {
    fn name(&self) -> &str {
        "multi-cell-power-provider"
    }
}

impl rdif_power::Interface for PowerProvider {
    fn power_on(&mut self, _id: rdif_power::PowerDomainId) -> Result<(), rdif_power::PowerError> {
        Ok(())
    }

    fn power_off(&mut self, _id: rdif_power::PowerDomainId) -> Result<(), rdif_power::PowerError> {
        Ok(())
    }
}

impl DriverGeneric for ResourceConsumer {
    fn name(&self) -> &str {
        "resource-consumer"
    }
}

fn probe_power_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_power::Power::new(PowerProvider));
    Ok(())
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .info()
        .prepare_resources(rdrive::probe::fdt::ResourcePrepareConfig::default())?;
    probe.into_platform_device().register(ResourceConsumer);
    Ok(())
}

static POWER_REGISTER: DriverRegister = DriverRegister {
    name: "test multi-cell power provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,multi-cell-power-provider"],
        on_probe: probe_power_provider,
    }],
};

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test multi-cell power consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,multi-cell-power-consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_resource_prepare_rejects_multi_cell_power_domain_specifier() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "power-controller",
            &[
                prop_strs("compatible", &["test,multi-cell-power-provider"]),
                prop_u32s("phandle", &[3]),
                prop_u32s("#power-domain-cells", &[2]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "mmc@16020000",
            &[
                prop_strs("compatible", &["test,multi-cell-power-consumer"]),
                prop_u32s("power-domains", &[3, 6, 7]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(POWER_REGISTER.clone());
    rdrive::register_add(CONSUMER_REGISTER.clone());

    let err = probe_all(true).expect_err("multi-cell power domains should fail probe");

    assert!(
        err.to_string().contains("uses 2 cells"),
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
