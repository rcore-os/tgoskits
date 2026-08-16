use core::ptr::NonNull;
use std::sync::Mutex;

use ax_driver::binding_info_from_fdt;
use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static CAPTURED_ERROR: Mutex<Option<String>> = Mutex::new(None);

static DEVICE_REGISTER: DriverRegister = DriverRegister {
    name: "relation-binding-device",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,relation-binding-device"],
        on_probe: validate_binding,
    }],
};

fn validate_binding(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let error = binding_info_from_fdt(probe.info())
        .expect_err("binding must reject an interrupt-parent without a registered Intc capability");
    *CAPTURED_ERROR.lock().unwrap() = Some(error.to_string());
    Ok(())
}

#[test]
fn fdt_relation_requires_an_available_interrupt_controller_provider() {
    let encoded = fdt_with_unregistered_interrupt_parent().encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform initializes");
    rdrive::register_add(DEVICE_REGISTER.clone());
    rdrive::probe_all(true).expect("probe callback handles the rejected binding");

    let error = CAPTURED_ERROR
        .lock()
        .unwrap()
        .clone()
        .expect("binding rejection captured");
    assert!(error.contains("not an available interrupt-controller provider"));
}

fn fdt_with_unregistered_interrupt_parent() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[1]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[1]));

    let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_strs("compatible", &["test,unregistered-intc"]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("phandle", &[1]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("#interrupt-cells", &[3]));

    let device = fdt.add_node(root, Node::new("device@0"));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_strs("compatible", &["test,relation-binding-device"]));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_u32s("interrupt-parent", &[1]));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_u32s("interrupts", &[0, 42, 4]));
    fdt
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
