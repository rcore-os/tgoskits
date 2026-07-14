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
use rdif_clk::{ClockId, KError};
use rdrive::{
    DriverGeneric, Platform, get_one,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static CLOCK_CALLS: Mutex<Vec<ClockCall>> = Mutex::new(Vec::new());
static ASSIGNED_APPLIED_BEFORE_PROBE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClockCall {
    operation: &'static str,
    id: usize,
    rate: Option<u64>,
}

struct ClockProviderDevice;
struct ScmiLikeClockProviderDevice;
struct ClockConsumerDevice;

impl DriverGeneric for ClockProviderDevice {
    fn name(&self) -> &str {
        "clock-provider"
    }
}

impl DriverGeneric for ScmiLikeClockProviderDevice {
    fn name(&self) -> &str {
        "scmi-like-clock-provider"
    }
}

impl rdif_clk::Interface for ClockProviderDevice {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: ClockId) -> Result<(), KError> {
        CLOCK_CALLS.lock().unwrap().push(ClockCall {
            operation: "enable",
            id: id.raw(),
            rate: None,
        });
        Ok(())
    }

    fn get_rate(&self, id: ClockId) -> Result<u64, KError> {
        Ok(24_000_000 + id.raw() as u64)
    }

    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError> {
        CLOCK_CALLS.lock().unwrap().push(ClockCall {
            operation: "set_rate",
            id: id.raw(),
            rate: Some(rate),
        });
        Ok(())
    }
}

impl DriverGeneric for ClockConsumerDevice {
    fn name(&self) -> &str {
        "clock-consumer"
    }
}

fn probe_clock_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_clk::Clk::new(ClockProviderDevice));
    Ok(())
}

fn probe_scmi_like_clock_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(ScmiLikeClockProviderDevice);
    Ok(())
}

fn probe_clock_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let lines = info.clock_lines()?;
    let core = info
        .find_clock_line_by_name("core")?
        .ok_or_else(|| OnProbeError::other("core clock line not found"))?;
    let bus = info
        .find_clock_line_by_name("bus")?
        .ok_or_else(|| OnProbeError::other("bus clock line not found"))?;

    ASSIGNED_APPLIED_BEFORE_PROBE.store(
        lines.len() == 2
            && core.name() == Some("core")
            && core.id() == ClockId::from(11)
            && bus.name() == Some("bus")
            && bus.id() == ClockId::from(12)
            && info.find_clock_line_by_name("utmi").unwrap().is_none()
            && CLOCK_CALLS.lock().unwrap().as_slice()
                == [ClockCall {
                    operation: "set_rate",
                    id: 11,
                    rate: Some(100_000_000),
                }],
        Ordering::SeqCst,
    );

    core.enable()?;
    bus.set_rate(50_000_000)?;
    assert_eq!(core.rate()?, 24_000_011);

    probe.into_platform_device().register(ClockConsumerDevice);
    Ok(())
}

static CLOCK_PROVIDER_REGISTER: DriverRegister = DriverRegister {
    name: "test clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,clock-provider"],
        on_probe: probe_clock_provider,
    }],
};

static SCMI_LIKE_CLOCK_PROVIDER_REGISTER: DriverRegister = DriverRegister {
    name: "test scmi-like clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,scmi-like-clock-provider"],
        on_probe: probe_scmi_like_clock_provider,
    }],
};

static CLOCK_CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test clock consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,clock-consumer"],
        on_probe: probe_clock_consumer,
    }],
};

#[test]
fn fdt_probe_applies_assigned_clock_rates_without_enabling_consumer_clocks() {
    CLOCK_CALLS.lock().unwrap().clear();
    ASSIGNED_APPLIED_BEFORE_PROBE.store(false, Ordering::SeqCst);

    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller",
            &[
                prop_strs("compatible", &["test,clock-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "scmi-clock-protocol",
            &[
                prop_strs("compatible", &["test,scmi-like-clock-provider"]),
                prop_u32s("phandle", &[2]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "usb-phy-clock-output",
            &[prop_u32s("phandle", &[3]), prop_u32s("#clock-cells", &[0])],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "unregistered-scmi-clock-protocol",
            &[prop_u32s("phandle", &[4]), prop_u32s("#clock-cells", &[1])],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "device@2000",
            &[
                prop_strs("compatible", &["test,clock-consumer"]),
                prop_u32s("clocks", &[1, 11, 1, 12, 3]),
                prop_strs("clock-names", &["core", "bus", "utmi"]),
                prop_u32s("assigned-clocks", &[1, 11, 1, 12, 2, 6, 4, 7]),
                prop_u32s(
                    "assigned-clock-rates",
                    &[100_000_000, 0, 200_000_000, 300_000_000],
                ),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(CLOCK_PROVIDER_REGISTER.clone());
    rdrive::register_add(SCMI_LIKE_CLOCK_PROVIDER_REGISTER.clone());
    rdrive::register_add(CLOCK_CONSUMER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert!(ASSIGNED_APPLIED_BEFORE_PROBE.load(Ordering::SeqCst));
    assert!(get_one::<ClockConsumerDevice>().is_some());
    assert_eq!(
        *CLOCK_CALLS.lock().unwrap(),
        vec![
            ClockCall {
                operation: "set_rate",
                id: 11,
                rate: Some(100_000_000),
            },
            ClockCall {
                operation: "enable",
                id: 11,
                rate: None,
            },
            ClockCall {
                operation: "set_rate",
                id: 12,
                rate: Some(50_000_000),
            },
        ]
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
