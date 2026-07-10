use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform, PlatformDevice,
    probe::{OnProbeError, fdt::ProbeFdt},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static ORDER_SEQ: AtomicUsize = AtomicUsize::new(0);
static PRE_KERNEL_ORDER: AtomicUsize = AtomicUsize::new(0);
static POST_KERNEL_ORDER: AtomicUsize = AtomicUsize::new(0);

struct LevelOrderDevice;

impl DriverGeneric for LevelOrderDevice {
    fn name(&self) -> &str {
        "LevelOrderDevice"
    }
}

fn string_list_property(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}

fn compatible_node(name: &str, compatible: &str) -> Node {
    let mut node = Node::new(name);
    node.set_property(string_list_property("compatible", &[compatible]));
    node
}

fn note_order(slot: &AtomicUsize) {
    let order = ORDER_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    slot.store(order, Ordering::SeqCst);
}

fn probe_pre_kernel(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&PRE_KERNEL_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(LevelOrderDevice);
    Ok(())
}

fn probe_post_kernel(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&POST_KERNEL_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(LevelOrderDevice);
    Ok(())
}

static POST_KERNEL_REGISTER: DriverRegister = DriverRegister {
    name: "post kernel level order test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,post-kernel"],
        on_probe: probe_post_kernel,
    }],
};

static PRE_KERNEL_REGISTER: DriverRegister = DriverRegister {
    name: "pre kernel level order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,pre-kernel"],
        on_probe: probe_pre_kernel,
    }],
};

#[test]
fn probe_all_keeps_probe_level_before_fdt_order() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        compatible_node("post-kernel@1000", "test,post-kernel"),
    );
    fdt.add_node(root, compatible_node("pre-kernel@2000", "test,pre-kernel"));

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");

    rdrive::register_add(POST_KERNEL_REGISTER.clone());
    rdrive::register_add(PRE_KERNEL_REGISTER.clone());

    rdrive::probe_all(true).expect("FDT probe should succeed");

    assert_eq!(PRE_KERNEL_ORDER.load(Ordering::SeqCst), 1);
    assert_eq!(POST_KERNEL_ORDER.load(Ordering::SeqCst), 2);
}
