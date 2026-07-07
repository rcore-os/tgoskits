use core::ptr::NonNull;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    vec,
    vec::Vec,
};

use fdt_edit::{Fdt, Node, Property};
use rdif_power::{PowerDomainId, PowerError};
use rdrive::{
    DriverGeneric, Platform, get_one,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static POWER_CALLS: Mutex<Vec<PowerCall>> = Mutex::new(Vec::new());
static POWERED_BEFORE_PROBE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq)]
struct PowerCall {
    operation: &'static str,
    id: u64,
}

struct PowerProviderDevice;
struct PowerConsumerDevice;

impl DriverGeneric for PowerProviderDevice {
    fn name(&self) -> &str {
        "power-provider"
    }
}

impl rdif_power::Interface for PowerProviderDevice {
    fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        POWER_CALLS.lock().unwrap().push(PowerCall {
            operation: "on",
            id: id.raw(),
        });
        Ok(())
    }

    fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        POWER_CALLS.lock().unwrap().push(PowerCall {
            operation: "off",
            id: id.raw(),
        });
        Ok(())
    }
}

impl DriverGeneric for PowerConsumerDevice {
    fn name(&self) -> &str {
        "power-consumer"
    }
}

fn probe_power_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_power::Power::new(PowerProviderDevice));
    Ok(())
}

fn probe_power_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let domain = probe
        .info()
        .find_power_domain_by_name("core")?
        .ok_or_else(|| OnProbeError::other("core power domain not found"))?;
    POWERED_BEFORE_PROBE.store(
        domain.specifier == vec![7]
            && POWER_CALLS.lock().unwrap().as_slice()
                == [PowerCall {
                    operation: "on",
                    id: 7,
                }],
        Ordering::SeqCst,
    );
    probe.into_platform_device().register(PowerConsumerDevice);
    Ok(())
}

static POWER_PROVIDER_REGISTER: DriverRegister = DriverRegister {
    name: "test power provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,power-provider"],
        on_probe: probe_power_provider,
    }],
};

static POWER_CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test power consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,power-consumer"],
        on_probe: probe_power_consumer,
    }],
};

#[test]
fn fdt_probe_powers_domains_before_consumer_probe() {
    POWER_CALLS.lock().unwrap().clear();
    POWERED_BEFORE_PROBE.store(false, Ordering::SeqCst);

    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "power-controller",
            &[
                prop_strs("compatible", &["test,power-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#power-domain-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "device@2000",
            &[
                prop_strs("compatible", &["test,power-consumer"]),
                prop_u32s("power-domains", &[1, 7]),
                prop_strs("power-domain-names", &["core"]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(POWER_PROVIDER_REGISTER.clone());
    rdrive::register_add(POWER_CONSUMER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert!(POWERED_BEFORE_PROBE.load(Ordering::SeqCst));
    assert!(get_one::<PowerConsumerDevice>().is_some());
    assert_eq!(
        *POWER_CALLS.lock().unwrap(),
        vec![PowerCall {
            operation: "on",
            id: 7,
        }]
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
