use core::ptr::NonNull;
use std::{
    string::{String, ToString},
    sync::Mutex,
    vec,
    vec::Vec,
};

use fdt_edit::{Fdt, Node, Phandle, Property};
use rdrive::{
    DriverGeneric, Platform, get,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static CAPTURED_RESET: Mutex<Option<CapturedReset>> = Mutex::new(None);

#[derive(Debug, PartialEq, Eq)]
struct CapturedReset {
    name: Option<String>,
    phandle: Phandle,
    cells: u32,
    specifier: Vec<u32>,
}

struct ResetProviderDevice;
struct ResetProviderAuxDevice;
struct ResetConsumerDevice;

impl DriverGeneric for ResetProviderDevice {
    fn name(&self) -> &str {
        "reset-provider"
    }
}

impl DriverGeneric for ResetProviderAuxDevice {
    fn name(&self) -> &str {
        "reset-provider-aux"
    }
}

impl DriverGeneric for ResetConsumerDevice {
    fn name(&self) -> &str {
        "reset-consumer"
    }
}

fn probe_reset_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let dev = probe.into_platform_device();
    dev.register(ResetProviderDevice);
    dev.register(ResetProviderAuxDevice);
    Ok(())
}

fn probe_reset_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let reset = info
        .find_reset_by_name("bus")?
        .ok_or_else(|| OnProbeError::other("bus reset not found"))?;
    let provider_id = info
        .phandle_to_device_id(reset.phandle)
        .ok_or_else(|| OnProbeError::other("reset provider has no device id"))?;

    get::<ResetProviderDevice>(provider_id)
        .map_err(|err| OnProbeError::other(format!("missing reset provider: {err}")))?;
    get::<ResetProviderAuxDevice>(provider_id)
        .map_err(|err| OnProbeError::other(format!("missing reset provider aux: {err}")))?;

    *CAPTURED_RESET.lock().unwrap() = Some(CapturedReset {
        name: reset.name,
        phandle: reset.phandle,
        cells: reset.cells,
        specifier: reset.specifier,
    });
    probe.into_platform_device().register(ResetConsumerDevice);
    Ok(())
}

static RESET_PROVIDER_REGISTER: DriverRegister = DriverRegister {
    name: "test reset provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,reset-provider"],
        on_probe: probe_reset_provider,
    }],
};

static RESET_CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test reset consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,reset-consumer"],
        on_probe: probe_reset_consumer,
    }],
};

#[test]
fn fdt_reset_refs_preserve_provider_phandle_names_and_specifiers() {
    *CAPTURED_RESET.lock().unwrap() = None;

    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "reset-controller@1000",
            &[
                prop_strs("compatible", &["test,reset-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#reset-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "device@2000",
            &[
                prop_strs("compatible", &["test,reset-consumer"]),
                prop_u32s("resets", &[1, 10, 1, 11]),
                prop_strs("reset-names", &["core", "bus"]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(RESET_PROVIDER_REGISTER.clone());
    rdrive::register_add(RESET_CONSUMER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert_eq!(
        *CAPTURED_RESET.lock().unwrap(),
        Some(CapturedReset {
            name: Some("bus".to_string()),
            phandle: Phandle::from(1),
            cells: 1,
            specifier: vec![11],
        })
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
