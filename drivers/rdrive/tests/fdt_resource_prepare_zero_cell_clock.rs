use core::ptr::NonNull;
use std::{
    string::{String, ToString},
    sync::Mutex,
    vec,
    vec::Vec,
};

use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static CLOCK_CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PREPARED_CLOCK: Mutex<Option<u64>> = Mutex::new(None);

struct ClockProvider;
struct ResourceConsumer;

impl DriverGeneric for ClockProvider {
    fn name(&self) -> &str {
        "zero-cell-clock-provider"
    }
}

impl rdif_clk::Interface for ClockProvider {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), rdrive::KError> {
        CLOCK_CALLS
            .lock()
            .unwrap()
            .push(format!("enable:{}", id.raw()));
        Ok(())
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        CLOCK_CALLS
            .lock()
            .unwrap()
            .push(format!("rate:{}", id.raw()));
        Ok(50_000_000)
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), rdrive::KError> {
        CLOCK_CALLS
            .lock()
            .unwrap()
            .push(format!("set:{}:{rate}", id.raw()));
        Ok(())
    }
}

impl DriverGeneric for ResourceConsumer {
    fn name(&self) -> &str {
        "resource-consumer"
    }
}

fn probe_clock_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_clk::Clk::new(ClockProvider));
    Ok(())
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let report = probe.info().prepare_resources(
        rdrive::probe::fdt::ResourcePrepareConfig::default().with_named_clock_rate("ciu"),
    )?;
    *PREPARED_CLOCK.lock().unwrap() = report.clock_rate("ciu");
    probe.into_platform_device().register(ResourceConsumer);
    Ok(())
}

static CLOCK_REGISTER: DriverRegister = DriverRegister {
    name: "test zero-cell clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,zero-cell-clock-provider"],
        on_probe: probe_clock_provider,
    }],
};

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test zero-cell clock resource consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,zero-cell-clock-resource-consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_resource_prepare_uses_id_zero_for_zero_cell_clock_provider() {
    CLOCK_CALLS.lock().unwrap().clear();
    *PREPARED_CLOCK.lock().unwrap() = None;

    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(
        root,
        node_with_props(
            "clock-controller",
            &[
                prop_strs("compatible", &["test,zero-cell-clock-provider"]),
                prop_u32s("phandle", &[1]),
                prop_u32s("#clock-cells", &[0]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "mmc@16020000",
            &[
                prop_strs("compatible", &["test,zero-cell-clock-resource-consumer"]),
                prop_u32s("clocks", &[1]),
                prop_strs("clock-names", &["ciu"]),
                prop_u32s("assigned-clocks", &[1]),
                prop_u32s("assigned-clock-rates", &[50_000_000]),
            ],
        ),
    );

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(CLOCK_REGISTER.clone());
    rdrive::register_add(CONSUMER_REGISTER.clone());

    probe_all(true).expect("zero-cell clock prepare should succeed");

    assert_eq!(
        *CLOCK_CALLS.lock().unwrap(),
        vec![
            "set:0:50000000".to_string(),
            "enable:0".to_string(),
            "rate:0".to_string(),
        ]
    );
    assert_eq!(*PREPARED_CLOCK.lock().unwrap(), Some(50_000_000));
    assert!(rdrive::get_one::<ResourceConsumer>().is_some());
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
