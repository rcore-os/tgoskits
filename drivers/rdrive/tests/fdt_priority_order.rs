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
static PARENT_INTC_ORDER: AtomicUsize = AtomicUsize::new(0);
static CHILD_INTC_ORDER: AtomicUsize = AtomicUsize::new(0);
static FIRST_INTC_ORDER: AtomicUsize = AtomicUsize::new(0);
static SECOND_INTC_ORDER: AtomicUsize = AtomicUsize::new(0);
static TIMER_ORDER: AtomicUsize = AtomicUsize::new(0);

struct PriorityOrderDevice;

impl DriverGeneric for PriorityOrderDevice {
    fn name(&self) -> &str {
        "PriorityOrderDevice"
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

fn u32_property(name: &str, value: u32) -> Property {
    Property::new(name, value.to_be_bytes().to_vec())
}

fn compatible_node(name: &str, compatible: &str, intc: Option<(u32, Option<u32>)>) -> Node {
    let mut node = Node::new(name);
    node.set_property(string_list_property("compatible", &[compatible]));
    if let Some((phandle, interrupt_parent)) = intc {
        node.set_property(u32_property("phandle", phandle));
        node.set_property(Property::new("interrupt-controller", Vec::new()));
        node.set_property(u32_property("#interrupt-cells", 1));
        if let Some(interrupt_parent) = interrupt_parent {
            node.set_property(u32_property("interrupt-parent", interrupt_parent));
        }
    }
    node
}

fn note_order(slot: &AtomicUsize) {
    let order = ORDER_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    slot.store(order, Ordering::SeqCst);
}

fn probe_first_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&FIRST_INTC_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(PriorityOrderDevice);
    Ok(())
}

fn probe_parent_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&PARENT_INTC_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(PriorityOrderDevice);
    Ok(())
}

fn probe_child_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&CHILD_INTC_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(PriorityOrderDevice);
    Ok(())
}

fn probe_second_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&SECOND_INTC_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(PriorityOrderDevice);
    Ok(())
}

fn probe_timer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    note_order(&TIMER_ORDER);
    let dev: PlatformDevice = probe.into_platform_device();
    dev.register(PriorityOrderDevice);
    Ok(())
}

static CHILD_INTC_REGISTER: DriverRegister = DriverRegister {
    name: "child intc order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,child-intc"],
        on_probe: probe_child_intc,
    }],
};

static PARENT_INTC_REGISTER: DriverRegister = DriverRegister {
    name: "parent intc order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,parent-intc"],
        on_probe: probe_parent_intc,
    }],
};

static SECOND_INTC_REGISTER: DriverRegister = DriverRegister {
    name: "second intc order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,second-intc"],
        on_probe: probe_second_intc,
    }],
};

static FIRST_INTC_REGISTER: DriverRegister = DriverRegister {
    name: "first intc order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,first-intc"],
        on_probe: probe_first_intc,
    }],
};

static TIMER_REGISTER: DriverRegister = DriverRegister {
    name: "timer order test driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::TIMER,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,timer"],
        on_probe: probe_timer,
    }],
};

#[test]
fn pre_kernel_priority_barrier_preserves_fdt_node_order_across_drivers() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        compatible_node("child-intc@0800", "test,child-intc", Some((2, Some(1)))),
    );
    fdt.add_node(
        root,
        compatible_node("parent-intc@0900", "test,parent-intc", Some((1, None))),
    );
    fdt.add_node(
        root,
        compatible_node("first-intc@1000", "test,first-intc", Some((3, None))),
    );
    fdt.add_node(
        root,
        compatible_node("second-intc@2000", "test,second-intc", Some((4, None))),
    );
    fdt.add_node(root, compatible_node("timer@3000", "test,timer", None));

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");

    rdrive::register_add(CHILD_INTC_REGISTER.clone());
    rdrive::register_add(SECOND_INTC_REGISTER.clone());
    rdrive::register_add(FIRST_INTC_REGISTER.clone());
    rdrive::register_add(PARENT_INTC_REGISTER.clone());
    rdrive::register_add(TIMER_REGISTER.clone());

    rdrive::probe_pre_kernel_until(ProbePriority::INTC, true)
        .expect("INTC barrier probe should succeed");

    assert_eq!(PARENT_INTC_ORDER.load(Ordering::SeqCst), 1);
    assert_eq!(FIRST_INTC_ORDER.load(Ordering::SeqCst), 2);
    assert_eq!(SECOND_INTC_ORDER.load(Ordering::SeqCst), 3);
    assert_eq!(CHILD_INTC_ORDER.load(Ordering::SeqCst), 4);
    assert_eq!(TIMER_ORDER.load(Ordering::SeqCst), 0);

    rdrive::probe_pre_kernel_until(ProbePriority::TIMER, true)
        .expect("TIMER barrier probe should succeed");

    assert_eq!(TIMER_ORDER.load(Ordering::SeqCst), 5);
}
