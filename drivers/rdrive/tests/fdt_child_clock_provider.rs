use core::ptr::NonNull;
use std::vec::Vec;

use fdt_edit::{Fdt, Node, Property, Status};
use rdrive::{
    DriverGeneric, Platform,
    error::FdtChildProviderError,
    get_list,
    probe::{OnProbeError, fdt::ProbeFdt},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

const SCMI_CLOCK_PROTOCOL_ID: u32 = 0x14;
const AUXILIARY_PROTOCOL_ID: u32 = 0x15;

struct ScmiTransport;

impl DriverGeneric for ScmiTransport {
    fn name(&self) -> &str {
        "test-scmi-transport"
    }
}

struct FakeClock {
    rate: u64,
}

impl DriverGeneric for FakeClock {
    fn name(&self) -> &str {
        "test-scmi-clock"
    }
}

impl rdif_clk::Interface for FakeClock {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, _id: rdif_clk::ClockId) -> Result<(), rdrive::KError> {
        Ok(())
    }

    fn get_rate(&self, _id: rdif_clk::ClockId) -> Result<u64, rdrive::KError> {
        Ok(self.rate)
    }

    fn set_rate(&mut self, _id: rdif_clk::ClockId, rate: u64) -> Result<(), rdrive::KError> {
        self.rate = rate;
        Ok(())
    }
}

struct AuxiliaryProtocol;

impl DriverGeneric for AuxiliaryProtocol {
    fn name(&self) -> &str {
        "test-scmi-auxiliary"
    }
}

struct ClockConsumer;

impl DriverGeneric for ClockConsumer {
    fn name(&self) -> &str {
        "clock-consumer"
    }
}

fn probe_scmi_transport(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let available_children = info.available_children();
    assert!(
        available_children
            .iter()
            .all(|child| !matches!(child.node().as_node().status(), Some(Status::Disabled)))
    );

    if info.node.path() == "/scmi@0" {
        let disabled = rdrive::probe::fdt::child_nodes(info.node)
            .into_iter()
            .find(|child| matches!(child.as_node().status(), Some(Status::Disabled)))
            .expect("test FDT has a disabled protocol child");
        assert!(matches!(
            info.prepare_child(disabled),
            Err(FdtChildProviderError::Disabled { .. })
        ));

        let unrelated = info
            .get_by_phandle(7.into())
            .expect("test FDT has an unrelated provider");
        assert!(matches!(
            info.prepare_child(unrelated),
            Err(FdtChildProviderError::NotDirectChild { .. })
        ));

        let nested = info
            .get_by_phandle(4.into())
            .expect("test FDT has a nested provider");
        assert!(matches!(
            info.prepare_child(nested),
            Err(FdtChildProviderError::NotDirectChild { .. })
        ));
    }

    let clock_child = available_children
        .iter()
        .find(|child| protocol_id(child.node()) == Some(SCMI_CLOCK_PROTOCOL_ID))
        .cloned()
        .expect("SCMI transport has an enabled clock protocol child");
    let rate = info
        .node
        .as_node()
        .get_property("test,clock-rate")
        .and_then(|property| property.get_u32())
        .expect("test transport has a clock rate") as u64;
    platform
        .register_with_fdt_child(
            ScmiTransport,
            clock_child,
            rdif_clk::Clk::new(FakeClock { rate }),
        )
        .map_err(|error| OnProbeError::other(error.to_string()))?;

    if let Some(auxiliary_child) = available_children
        .iter()
        .find(|child| protocol_id(child.node()) == Some(AUXILIARY_PROTOCOL_ID))
        .cloned()
    {
        assert!(auxiliary_child.node().as_node().phandle().is_none());
        platform
            .register_fdt_child(auxiliary_child.clone(), AuxiliaryProtocol)
            .map_err(|error| OnProbeError::other(error.to_string()))?;
        assert!(matches!(
            platform.register_fdt_child(auxiliary_child, AuxiliaryProtocol),
            Err(FdtChildProviderError::DuplicateCapability { .. })
        ));
    }
    Ok(())
}

fn probe_clock_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let expected_rate = probe
        .info()
        .node
        .as_node()
        .get_property("test,expected-rate")
        .and_then(|property| property.get_u32())
        .expect("test consumer has an expected rate") as u64;
    let clock = probe
        .info()
        .find_clock_line_by_name("ciu")?
        .ok_or_else(|| OnProbeError::other("ciu clock was not resolved"))?;
    clock.enable()?;
    assert_eq!(clock.rate()?, expected_rate);
    clock.set_rate(expected_rate + 1)?;
    assert_eq!(clock.rate()?, expected_rate + 1);
    probe.into_platform_device().register(ClockConsumer);
    Ok(())
}

fn protocol_id(node: fdt_edit::NodeType<'_>) -> Option<u32> {
    node.as_node()
        .get_property("reg")
        .and_then(|property| property.get_u32())
}

static SCMI_TRANSPORT_REGISTER: DriverRegister = DriverRegister {
    name: "test SCMI transport",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,scmi-transport"],
        on_probe: probe_scmi_transport,
    }],
};

static CLOCK_CONSUMER_REGISTER: DriverRegister = DriverRegister {
    name: "test child clock consumer",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,clock-consumer"],
        on_probe: probe_clock_consumer,
    }],
};

#[test]
fn child_clock_providers_preserve_fdt_identity_and_backend_ownership() {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    let transport_a = add_transport(&mut fdt, root, "scmi@0", 1, 2, 50_000_000);
    let clock_a = fdt
        .get_by_path("/scmi@0/protocol@14")
        .expect("clock protocol A exists")
        .id();
    fdt.add_node(
        clock_a,
        node_with_props("nested", &[prop_u32s("phandle", &[4])]),
    );
    fdt.add_node(
        transport_a,
        node_with_props("protocol@15", &[prop_u32s("reg", &[AUXILIARY_PROTOCOL_ID])]),
    );
    fdt.add_node(
        transport_a,
        node_with_props(
            "protocol@99",
            &[
                prop_u32s("reg", &[0x99]),
                prop_u32s("phandle", &[3]),
                prop_strs("status", &["disabled"]),
            ],
        ),
    );

    add_transport(&mut fdt, root, "scmi@1", 5, 6, 75_000_000);
    fdt.add_node(
        root,
        node_with_props("unrelated", &[prop_u32s("phandle", &[7])]),
    );
    add_consumer(&mut fdt, root, "mmc@0", 2, 50_000_000);
    add_consumer(&mut fdt, root, "mmc@1", 6, 75_000_000);

    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).expect("encoded FDT address is non-null"),
    })
    .expect("FDT platform should initialize");
    rdrive::register_add(SCMI_TRANSPORT_REGISTER.clone());
    rdrive::register_add(CLOCK_CONSUMER_REGISTER.clone());

    probe_all(true).expect("SCMI child clock providers should satisfy both consumers");

    assert_eq!(get_list::<ScmiTransport>().len(), 2);
    assert_eq!(get_list::<ClockConsumer>().len(), 2);
    let auxiliary_id = rdrive::fdt_path_to_device_id("/scmi@0/protocol@15")
        .expect("no-phandle auxiliary protocol has a stable device identity");
    let auxiliary = rdrive::get::<AuxiliaryProtocol>(auxiliary_id)
        .expect("no-phandle auxiliary protocol capability");
    assert_eq!(auxiliary.descriptor().device_id(), auxiliary_id);
    assert_eq!(
        auxiliary
            .descriptor()
            .fdt_node()
            .expect("auxiliary protocol FDT identity")
            .path(),
        "/scmi@0/protocol@15"
    );
    assert!(rdrive::fdt_path_to_device_id("/scmi@0/protocol@99").is_none());
    assert!(rdrive::fdt_path_to_device_id("/unrelated").is_none());

    let clock_a_id = rdrive::fdt_phandle_to_device_id(2.into()).expect("clock A identity");
    let clock_b_id = rdrive::fdt_phandle_to_device_id(6.into()).expect("clock B identity");
    assert_eq!(
        rdrive::get::<rdif_clk::Clk>(clock_a_id)
            .expect("clock A capability")
            .descriptor()
            .fdt_node()
            .expect("clock A FDT identity")
            .path(),
        "/scmi@0/protocol@14"
    );
    assert_eq!(
        rdrive::get::<rdif_clk::Clk>(clock_b_id)
            .expect("clock B capability")
            .descriptor()
            .fdt_node()
            .expect("clock B FDT identity")
            .path(),
        "/scmi@1/protocol@14"
    );
}

fn add_transport(
    fdt: &mut Fdt,
    root: fdt_edit::NodeId,
    name: &str,
    transport_phandle: u32,
    clock_phandle: u32,
    clock_rate: u32,
) -> fdt_edit::NodeId {
    let transport = fdt.add_node(
        root,
        node_with_props(
            name,
            &[
                prop_strs("compatible", &["test,scmi-transport"]),
                prop_u32s("phandle", &[transport_phandle]),
                prop_u32s("#address-cells", &[1]),
                prop_u32s("#size-cells", &[0]),
                prop_u32s("test,clock-rate", &[clock_rate]),
            ],
        ),
    );
    fdt.add_node(
        transport,
        node_with_props(
            "protocol@14",
            &[
                prop_u32s("reg", &[SCMI_CLOCK_PROTOCOL_ID]),
                prop_u32s("phandle", &[clock_phandle]),
                prop_u32s("#clock-cells", &[1]),
            ],
        ),
    );
    transport
}

fn add_consumer(fdt: &mut Fdt, root: fdt_edit::NodeId, name: &str, phandle: u32, rate: u32) {
    fdt.add_node(
        root,
        node_with_props(
            name,
            &[
                prop_strs("compatible", &["test,clock-consumer"]),
                prop_u32s("clocks", &[phandle, 3]),
                prop_strs("clock-names", &["ciu"]),
                prop_u32s("test,expected-rate", &[rate]),
            ],
        ),
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
