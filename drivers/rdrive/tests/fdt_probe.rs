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

static FAIL_PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
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

fn probe_serial_not_match(_probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    FAIL_PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    Err(OnProbeError::NotMatch)
}

static FDT_SERIAL_NOT_MATCH_REGISTER: DriverRegister = DriverRegister {
    name: "fdt serial negative test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,uart", "ns16550a"],
        on_probe: probe_serial_not_match,
    }],
};

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
    let aliases = fdt.add_node(root, Node::new("aliases"));
    fdt.node_mut(aliases)
        .unwrap()
        .set_property(string_property("serial0", "/serial@1000"));
    let chosen = fdt.add_node(root, Node::new("chosen"));
    fdt.node_mut(chosen)
        .unwrap()
        .set_property(string_property("stdout-path", "serial0:115200n8"));
    fdt.add_node(root, serial_node("serial@1000", true));
    fdt.add_node(root, serial_node("serial@2000", true));
    fdt.add_node(root, serial_node("serial@3000", false));

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(FDT_SERIAL_NOT_MATCH_REGISTER.clone());
    rdrive::register_add(FDT_SERIAL_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert_eq!(FAIL_PROBE_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(PROBE_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(get_list::<FdtSerialDevice>().len(), 2);
    assert!(rdrive::fdt_path_to_device_id("/serial@1000").is_some());
    assert!(rdrive::fdt_path_to_device_id("/serial@2000").is_some());
    assert!(rdrive::fdt_path_to_device_id("/serial@3000").is_none());
    let serial0_device =
        rdrive::fdt_path_to_device_id("/serial@1000").expect("enabled serial probed");
    assert!(rdrive::note_fdt_device_path("/serial@3000", serial0_device));
    assert_eq!(
        rdrive::fdt_path_to_device_id("/serial@3000"),
        Some(serial0_device)
    );
    assert!(!rdrive::note_fdt_device_path("/missing@0", serial0_device));

    let stdout_path = rdrive::with_fdt(|fdt| {
        fdt.get_by_path("/chosen")
            .and_then(|chosen| {
                chosen
                    .as_node()
                    .get_property("stdout-path")
                    .and_then(|prop| prop.as_str())
            })
            .and_then(|stdout| stdout.split(':').next())
            .and_then(|alias| {
                fdt.get_by_path("/aliases").and_then(|aliases| {
                    aliases
                        .as_node()
                        .get_property(alias)
                        .and_then(|prop| prop.as_str())
                })
            })
            .map(str::to_owned)
    })
    .flatten()
    .expect("stdout-path should resolve through aliases");

    assert!(rdrive::fdt_path_to_device_id(&stdout_path).is_some());
}
