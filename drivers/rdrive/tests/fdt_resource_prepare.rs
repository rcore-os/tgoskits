use core::ptr::NonNull;
use std::{
    string::{String, ToString},
    sync::Mutex,
    vec,
    vec::Vec,
};

use fdt_edit::{Fdt, Node, NodeType, Phandle, Property};
use rdif_pinctrl::{
    ConfigSetting, FdtPinctrlParser, GpioBankId, GpioLineId, GpioRange,
    Interface as PinctrlInterface, PinDesc, PinId, PinState, PinctrlDevice, PinctrlError,
};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static RESOURCE_CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PREPARED_CLOCK: Mutex<Option<u64>> = Mutex::new(None);

struct ClockProvider;
struct ResetProvider;
struct PowerProvider;
struct PinctrlProvider {
    pins: Vec<PinDesc>,
    gpio_ranges: Vec<GpioRange>,
}
struct PinctrlParser;
struct ResourceConsumer;

impl PinctrlProvider {
    fn new() -> Self {
        Self {
            pins: (0..64)
                .map(|pin| PinDesc::new(PinId::new(pin), None))
                .collect(),
            gpio_ranges: vec![GpioRange::new(GpioBankId::new(0), 0, 0, 64)],
        }
    }
}

impl DriverGeneric for ClockProvider {
    fn name(&self) -> &str {
        "clock-provider"
    }
}

impl rdif_clk::Interface for ClockProvider {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), rdrive::KError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("clock-enable:{}", id.raw()));
        Ok(())
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("clock-rate:{}", id.raw()));
        Ok(50_000_000)
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), rdrive::KError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("clock-set:{}:{rate}", id.raw()));
        Ok(())
    }
}

impl DriverGeneric for ResetProvider {
    fn name(&self) -> &str {
        "reset-provider"
    }
}

impl rdif_reset::Interface for ResetProvider {
    fn assert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("reset-assert:{}", id.raw()));
        Ok(())
    }

    fn deassert(&mut self, id: rdif_reset::ResetId) -> Result<(), rdif_reset::ResetError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("reset-deassert:{}", id.raw()));
        Ok(())
    }
}

impl DriverGeneric for PowerProvider {
    fn name(&self) -> &str {
        "power-provider"
    }
}

impl rdif_power::Interface for PowerProvider {
    fn power_on(&mut self, id: rdif_power::PowerDomainId) -> Result<(), rdif_power::PowerError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push(format!("power-on:{}", id.raw()));
        Ok(())
    }

    fn power_off(&mut self, _id: rdif_power::PowerDomainId) -> Result<(), rdif_power::PowerError> {
        Ok(())
    }
}

impl DriverGeneric for PinctrlProvider {
    fn name(&self) -> &str {
        "pinctrl-provider"
    }
}

impl PinctrlInterface for PinctrlProvider {
    fn pins(&self) -> &[PinDesc] {
        &self.pins
    }

    fn gpio_ranges(&self) -> &[GpioRange] {
        &self.gpio_ranges
    }

    fn apply_config(&mut self, _setting: &ConfigSetting) -> Result<(), PinctrlError> {
        RESOURCE_CALLS
            .lock()
            .unwrap()
            .push("pinctrl-config".to_string());
        Ok(())
    }
}

impl FdtPinctrlParser for PinctrlParser {
    fn parse_pinctrl_node(
        &self,
        _fdt: &Fdt,
        _node: NodeType<'_>,
        _state: &mut PinState,
    ) -> Result<(), PinctrlError> {
        Err(PinctrlError::InvalidConfig)
    }

    fn parse_gpio_line(
        &self,
        fdt: &Fdt,
        consumer: &Node,
        prop_name: &str,
    ) -> Option<Result<GpioLineId, PinctrlError>> {
        let mut cells = consumer.get_property(prop_name)?.get_u32_iter();
        let phandle = Phandle::from(cells.next()?);
        let offset = cells.next()?;
        fdt.get_by_phandle(phandle)?;
        Some(Ok(GpioLineId::new(GpioBankId::new(0), offset)))
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

fn probe_reset_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_reset::Reset::new(ResetProvider));
    Ok(())
}

fn probe_power_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_power::Power::new(PowerProvider));
    Ok(())
}

fn probe_pinctrl_provider(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(PinctrlDevice::with_fdt_parser(
            PinctrlProvider::new(),
            PinctrlParser,
        ));
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
    name: "test clock provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,clock-provider"],
        on_probe: probe_clock_provider,
    }],
};

static RESET_REGISTER: DriverRegister = DriverRegister {
    name: "test reset provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,reset-provider"],
        on_probe: probe_reset_provider,
    }],
};

static POWER_REGISTER: DriverRegister = DriverRegister {
    name: "test power provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,power-provider"],
        on_probe: probe_power_provider,
    }],
};

static PINCTRL_REGISTER: DriverRegister = DriverRegister {
    name: "test pinctrl provider",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,pinctrl-provider"],
        on_probe: probe_pinctrl_provider,
    }],
};

static CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test resource consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,resource-consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn fdt_resource_prepare_applies_clocks_resets_and_power_domains() {
    RESOURCE_CALLS.lock().unwrap().clear();
    *PREPARED_CLOCK.lock().unwrap() = None;

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
        node_with_props("gpio0@0", &[prop_u32s("phandle", &[20])]),
    );
    fdt.add_node(
        root,
        node_with_props(
            "pinctrl",
            &[prop_strs("compatible", &["test,pinctrl-provider"])],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "vmmc-regulator",
            &[
                prop_strs("compatible", &["regulator-fixed"]),
                prop_u32s("phandle", &[4]),
                prop_u32s("gpios", &[20, 5, 0]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "vqmmc-regulator",
            &[
                prop_strs("compatible", &["regulator-fixed"]),
                prop_u32s("phandle", &[5]),
                prop_u32s("gpios", &[20, 6, 0]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "reset-controller",
            &[
                prop_strs("compatible", &["test,reset-provider"]),
                prop_u32s("phandle", &[2]),
                prop_u32s("#reset-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "power-controller",
            &[
                prop_strs("compatible", &["test,power-provider"]),
                prop_u32s("phandle", &[3]),
                prop_u32s("#power-domain-cells", &[1]),
            ],
        ),
    );
    fdt.add_node(
        root,
        node_with_props(
            "mmc@16020000",
            &[
                prop_strs("compatible", &["test,resource-consumer"]),
                prop_u32s("clocks", &[1, 4]),
                prop_strs("clock-names", &["ciu"]),
                prop_u32s("assigned-clocks", &[1, 4]),
                prop_u32s("assigned-clock-rates", &[50_000_000]),
                prop_u32s("vmmc-supply", &[4]),
                prop_u32s("vqmmc-supply", &[5]),
                prop_u32s("resets", &[2, 9]),
                prop_u32s("power-domains", &[3, 6]),
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
    rdrive::register_add(RESET_REGISTER.clone());
    rdrive::register_add(POWER_REGISTER.clone());
    rdrive::register_add(PINCTRL_REGISTER.clone());
    rdrive::register_add(CONSUMER_REGISTER.clone());

    probe_all(true).expect("FDT probe should succeed");

    assert_eq!(
        *RESOURCE_CALLS.lock().unwrap(),
        vec![
            "power-on:6".to_string(),
            "clock-set:4:50000000".to_string(),
            "pinctrl-config".to_string(),
            "pinctrl-config".to_string(),
            "pinctrl-config".to_string(),
            "pinctrl-config".to_string(),
            "clock-enable:4".to_string(),
            "reset-deassert:9".to_string(),
            "clock-rate:4".to_string(),
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
