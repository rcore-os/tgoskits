use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static TRANSPORT_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

struct ExistingChildCapability;

impl DriverGeneric for ExistingChildCapability {
    fn name(&self) -> &str {
        "existing-child-capability"
    }
}

struct AtomicParent;

impl DriverGeneric for AtomicParent {
    fn name(&self) -> &str {
        "atomic-parent"
    }
}

struct ReplacementChildCapability;

impl DriverGeneric for ReplacementChildCapability {
    fn name(&self) -> &str {
        "replacement-child-capability"
    }
}

fn probe_existing_child(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(ExistingChildCapability);
    Ok(())
}

fn probe_transport(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    TRANSPORT_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    let (info, platform) = probe.into_parts();
    let child = info
        .available_children()
        .into_iter()
        .next()
        .expect("test transport has an available child");
    platform
        .register_with_fdt_child(AtomicParent, child, ReplacementChildCapability)
        .map_err(|error| OnProbeError::other(error.to_string()))
}

static EXISTING_CHILD_REGISTER: DriverRegister = DriverRegister {
    name: "existing child provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,existing-provider"],
        on_probe: probe_existing_child,
    }],
};

static TRANSPORT_REGISTER: DriverRegister = DriverRegister {
    name: "atomic transport",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,atomic-transport"],
        on_probe: probe_transport,
    }],
};

#[test]
fn failed_child_publication_leaves_parent_unpublished_and_is_retry_safe() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    let transport = fdt.add_node(
        root,
        node_with_props(
            "transport",
            &[prop_strs("compatible", &["test,atomic-transport"])],
        ),
    );
    fdt.add_node(
        transport,
        node_with_props(
            "provider",
            &[
                prop_strs("compatible", &["test,existing-provider"]),
                prop_u32s("phandle", &[2]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).expect("encoded FDT address is non-null"),
    })
    .expect("FDT platform should initialize");

    let existing = rdrive::probe::fdt::probe_register(&EXISTING_CHILD_REGISTER)
        .expect("existing provider probe should run");
    assert!(existing.into_iter().all(|result| result.is_ok()));

    for expected_attempts in 1..=2 {
        let transport = rdrive::probe::fdt::probe_register(&TRANSPORT_REGISTER)
            .expect("transport probe should run");
        assert_eq!(transport.len(), 1);
        let error = transport
            .into_iter()
            .next()
            .expect("transport probe result")
            .expect_err("pre-populated child must reject transport ownership");
        assert!(format!("{error}").contains("already populated"));
        assert_eq!(TRANSPORT_ATTEMPTS.load(Ordering::SeqCst), expected_attempts);
        assert!(rdrive::get_one::<AtomicParent>().is_none());
        assert!(rdrive::get_one::<ReplacementChildCapability>().is_none());
        assert!(rdrive::fdt_path_to_device_id("/transport").is_none());
    }

    assert!(rdrive::get_one::<ExistingChildCapability>().is_some());
    assert!(rdrive::fdt_path_to_device_id("/transport/provider").is_some());
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
