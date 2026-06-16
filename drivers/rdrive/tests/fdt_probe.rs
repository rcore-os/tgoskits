use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform, PlatformDevice, get_list,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);

struct FdtSerialDevice;

impl DriverGeneric for FdtSerialDevice {
    fn name(&self) -> &str {
        "FdtSerialDevice"
    }
}

fn string_property(name: &str, value: &str) -> Property {
    let mut data = value.as_bytes().to_vec();
    data.push(0);
    Property::new(name, data)
}

fn string_list_property(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}

fn serial_node(name: &str, enabled: bool) -> Node {
    let mut node = Node::new(name);
    node.set_property(string_list_property(
        "compatible",
        &["test,uart", "ns16550a"],
    ));
    if !enabled {
        node.set_property(string_property("status", "disabled"));
    }
    node
}

fn probe_serial(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(FdtSerialDevice);
    Ok(())
}

static FDT_SERIAL_REGISTER: DriverRegister = DriverRegister {
    name: "fdt serial test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,uart", "ns16550a"],
        on_probe: probe_serial,
    }],
};

#[test]
fn fdt_probe_populates_each_enabled_matching_node_once() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(root, serial_node("serial@1000", true));
    fdt.add_node(root, serial_node("serial@2000", true));
    fdt.add_node(root, serial_node("serial@3000", false));

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(FDT_SERIAL_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert_eq!(PROBE_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(get_list::<FdtSerialDevice>().len(), 2);
}
