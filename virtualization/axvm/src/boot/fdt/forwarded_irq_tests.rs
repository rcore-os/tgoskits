use alloc::{vec, vec::Vec};

use axvm_types::{VMBootProtocol, VMInterruptMode};
use axvmconfig::AxVMCrateConfig;
use fdt_edit::{Fdt, Node, Property};

use super::test_core::{
    forwarded_irq::discover_aarch64_hybrid_routes, prepare_dtb_guest_with_host_fdt,
};
use crate::{
    AxVmError, ForwardedIrqConfigError,
    boot::{BootImageProvider, StaticVmImage},
    config::{AxVMConfig, AxVMConfigParams, PassThroughDeviceConfig, PhysCpuList},
};

#[test]
fn selected_host_node_is_the_only_forwarded_irq_producer() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    let intc = host_fdt.add_node(root, interrupt_controller());
    let selected = host_fdt.add_node(root, interrupt_device("selected@1000", 1, 5));
    let _unselected = host_fdt.add_node(root, interrupt_device("unselected@2000", 1, 6));
    assert_eq!(host_fdt.path_of(intc), "/interrupt-controller@8000000");
    assert_eq!(host_fdt.path_of(selected), "/selected@1000");

    let mut config = AxVMConfig::default_for_test(1, "hybrid");
    config.add_pass_through_device(PassThroughDeviceConfig {
        name: "/selected@1000".into(),
        ..Default::default()
    });

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].host_spi_offset(), 5);
    assert_eq!(routes[0].guest_intid(), 37);
}

#[test]
fn invalid_selection_returns_typed_path_error() {
    let host_fdt = host_fdt_with_controller();
    let mut config = AxVMConfig::default_for_test(1, "hybrid");
    config.add_pass_through_device(PassThroughDeviceConfig {
        name: "serial@1000".into(),
        ..Default::default()
    });

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert_eq!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::InvalidSelection {
                selection: "serial@1000".into(),
            },
        }
    );
}

#[test]
fn unknown_interrupt_parent_keeps_node_phandle_and_raw_cells() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    host_fdt.add_node(root, interrupt_device("serial@1000", 99, 8));
    let config = config_with_selected_path("/serial@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert_eq!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::UnknownController {
                node: "/serial@1000".into(),
                phandle: 99,
                raw: vec![0, 8, 4],
            },
        }
    );
}

#[test]
fn interrupts_inherit_parent_from_ancestor_bus() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut bus = Node::new("soc");
    bus.add_property(cells_property("interrupt-parent", &[1]));
    let bus = host_fdt.add_node(root, bus);
    let mut serial = Node::new("serial@1000");
    serial.add_property(cells_property("interrupts", &[0, 9, 4]));
    host_fdt.add_node(bus, serial);
    let config = config_with_selected_path("/soc/serial@1000");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes[0].guest_intid(), 41);
}

#[test]
fn direct_parent_interrupt_controller_does_not_require_phandle() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    let mut controller = interrupt_controller();
    controller.remove_property("phandle");
    let controller = host_fdt.add_node(root, controller);
    let mut serial = Node::new("serial@1000");
    serial.add_property(cells_property("interrupts", &[0, 18, 4]));
    host_fdt.add_node(controller, serial);
    let config = config_with_selected_path("/interrupt-controller@8000000/serial@1000");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].guest_intid(), 50);
}

#[test]
fn natural_parent_controller_wins_over_its_upstream_interrupt_parent() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut gpio = Node::new("gpio@3000");
    gpio.add_property(string_property("compatible", "vendor,gpio-intc"));
    gpio.add_property(Property::new("interrupt-controller", Vec::new()));
    gpio.add_property(cells_property("#interrupt-cells", &[3]));
    gpio.add_property(cells_property("interrupt-parent", &[1]));
    let gpio = host_fdt.add_node(root, gpio);
    let mut child = Node::new("child@0");
    child.add_property(cells_property("interrupts", &[0, 21, 4]));
    host_fdt.add_node(gpio, child);
    let config = config_with_selected_path("/gpio@3000/child@0");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::UnsupportedController {
                controller,
                phandle: None,
                ..
            },
        } if controller == "/gpio@3000"
    ));
}

#[test]
fn interrupts_extended_uses_referenced_controller_cell_count() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut serial = Node::new("serial@1000");
    serial.add_property(cells_property("interrupts-extended", &[1, 0, 10, 4]));
    host_fdt.add_node(root, serial);
    let config = config_with_selected_path("/serial@1000");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes[0].guest_intid(), 42);
}

#[test]
fn selected_subtree_excludes_configured_descendants() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let soc = host_fdt.add_node(root, Node::new("soc"));
    host_fdt.add_node(soc, interrupt_device("serial@1000", 1, 11));
    host_fdt.add_node(soc, interrupt_device("gpio@2000", 1, 12));
    let config = config_with_selection_and_exclusion("/soc", "/soc/gpio@2000");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].guest_intid(), 43);
}

#[test]
fn passthrough_dependency_with_spi_is_also_a_route_producer() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut clock = interrupt_device("clock@2000", 1, 20);
    clock.add_property(cells_property("phandle", &[2]));
    clock.add_property(cells_property("#clock-cells", &[0]));
    host_fdt.add_node(root, clock);
    let mut serial = interrupt_device("serial@1000", 1, 19);
    serial.add_property(cells_property("clocks", &[2]));
    host_fdt.add_node(root, serial);
    let config = config_with_selected_path("/serial@1000");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();
    let mut guest_intids = routes
        .iter()
        .map(|route| route.guest_intid())
        .collect::<Vec<_>>();
    guest_intids.sort_unstable();

    assert_eq!(guest_intids, vec![51, 52]);
}

#[test]
fn whole_tree_skips_architectural_ppis_and_controller_irqs() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    host_fdt.add_node(root, interrupt_device("serial@1000", 1, 13));
    let mut timer = Node::new("timer");
    timer.add_property(string_property("compatible", "arm,armv8-timer"));
    timer.add_property(cells_property("interrupt-parent", &[1]));
    timer.add_property(cells_property("interrupts", &[1, 13, 4]));
    host_fdt.add_node(root, timer);
    let config = config_with_selected_path("/");

    let routes = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap();

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].guest_intid(), 45);
}

#[test]
fn ordinary_selected_ppi_is_rejected() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut device = Node::new("device@1000");
    device.add_property(cells_property("interrupt-parent", &[1]));
    device.add_property(cells_property("interrupts", &[1, 7, 4]));
    host_fdt.add_node(root, device);
    let config = config_with_selected_path("/device@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert_eq!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::UnsupportedGicSource {
                node: "/device@1000".into(),
                controller: "/interrupt-controller@8000000".into(),
                phandle: Some(1),
                raw: vec![1, 7, 4],
            },
        }
    );
}

#[test]
fn missing_interrupt_cells_is_reported_with_controller_context() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    let mut controller = interrupt_controller();
    controller.remove_property("#interrupt-cells");
    host_fdt.add_node(root, controller);
    host_fdt.add_node(root, interrupt_device("serial@1000", 1, 14));
    let config = config_with_selected_path("/serial@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::MissingInterruptCells {
                node,
                controller,
                phandle: Some(1),
                raw,
            },
        } if node == "/serial@1000"
            && controller == "/interrupt-controller@8000000"
            && raw == vec![0, 14, 4]
    ));
}

#[test]
fn zero_interrupt_cells_is_rejected_instead_of_panicking() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    let mut controller = interrupt_controller();
    controller.set_property(cells_property("#interrupt-cells", &[0]));
    host_fdt.add_node(root, controller);
    host_fdt.add_node(root, interrupt_device("serial@1000", 1, 14));
    let config = config_with_selected_path("/serial@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::MissingInterruptCells {
                node,
                controller,
                phandle: Some(1),
                raw,
            },
        } if node == "/serial@1000"
            && controller == "/interrupt-controller@8000000"
            && raw == vec![0, 14, 4]
    ));
}

#[test]
fn truncated_extended_tuple_is_rejected() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    let mut serial = Node::new("serial@1000");
    serial.add_property(cells_property("interrupts-extended", &[1, 0, 15]));
    host_fdt.add_node(root, serial);
    let config = config_with_selected_path("/serial@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::TruncatedSpecifier {
                node,
                phandle: Some(1),
                raw,
                ..
            },
        } if node == "/serial@1000" && raw == vec![0, 15]
    ));
}

#[test]
fn unsupported_interrupt_controller_is_rejected() {
    let mut host_fdt = Fdt::new();
    let root = host_fdt.root_id();
    let mut controller = interrupt_controller();
    controller.set_property(string_property("compatible", "vendor,other-intc"));
    host_fdt.add_node(root, controller);
    host_fdt.add_node(root, interrupt_device("serial@1000", 1, 16));
    let config = config_with_selected_path("/serial@1000");

    let error = discover_aarch64_hybrid_routes(&config, &host_fdt).unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::UnsupportedController {
                node,
                compatible,
                raw,
                ..
            },
        } if node == "/serial@1000"
            && compatible == "vendor,other-intc"
            && raw == vec![0, 16, 4]
    ));
}

#[test]
fn uefi_validates_hybrid_selection_before_skipping_guest_dtb() {
    let host_dtb = host_fdt_with_controller().encode();
    let mut config = hybrid_config_with_selected_path("serial@1000");
    let mut crate_config = uefi_crate_config();

    let error = prepare_dtb_guest_with_host_fdt(
        &mut config,
        &mut crate_config,
        &EmptyProvider,
        Some(host_dtb.as_ref()),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        AxVmError::ForwardedIrqConfig {
            source: ForwardedIrqConfigError::InvalidSelection { selection },
        } if selection == "serial@1000"
    ));
}

#[test]
fn uefi_keeps_host_discovered_hybrid_route_without_guest_dtb() {
    let mut host_fdt = host_fdt_with_controller();
    let root = host_fdt.root_id();
    host_fdt.add_node(root, interrupt_device("serial@1000", 1, 17));
    let host_dtb = host_fdt.encode();
    let mut config = hybrid_config_with_selected_path("/serial@1000");
    let mut crate_config = uefi_crate_config();

    let guest_dtb = prepare_dtb_guest_with_host_fdt(
        &mut config,
        &mut crate_config,
        &EmptyProvider,
        Some(host_dtb.as_ref()),
    )
    .unwrap();

    assert!(guest_dtb.is_none());
    assert_eq!(config.aarch64_hybrid_forwarded_irqs().len(), 1);
    assert_eq!(config.aarch64_hybrid_forwarded_irqs()[0].guest_intid(), 49);
}

#[test]
fn hybrid_requires_host_fdt_even_when_uefi_skips_guest_dtb() {
    let mut config = hybrid_config_with_selected_path("/serial@1000");
    let mut crate_config = uefi_crate_config();

    let error =
        prepare_dtb_guest_with_host_fdt(&mut config, &mut crate_config, &EmptyProvider, None)
            .unwrap_err();

    assert!(matches!(
        error,
        AxVmError::Unsupported {
            operation: "discover AArch64 Hybrid IRQ routes",
            ..
        }
    ));
}

struct EmptyProvider;

impl BootImageProvider for EmptyProvider {
    fn static_vm_images(&self) -> &'static [StaticVmImage] {
        &[]
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn read_file(&self, _file_name: &str) -> crate::AxVmResult<Vec<u8>> {
        Err(AxVmError::Unsupported {
            operation: "read test image",
            detail: "empty provider".into(),
        })
    }
}

fn interrupt_controller() -> Node {
    let mut node = Node::new("interrupt-controller@8000000");
    node.add_property(string_property("compatible", "arm,gic-v3"));
    node.add_property(cells_property("phandle", &[1]));
    node.add_property(cells_property("#interrupt-cells", &[3]));
    node.add_property(Property::new("interrupt-controller", Vec::new()));
    node
}

fn host_fdt_with_controller() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.add_node(root, interrupt_controller());
    fdt
}

fn config_with_selected_path(path: &str) -> AxVMConfig {
    let mut config = AxVMConfig::default_for_test(1, "hybrid");
    config.add_pass_through_device(PassThroughDeviceConfig {
        name: path.into(),
        ..Default::default()
    });
    config
}

fn hybrid_config_with_selected_path(path: &str) -> AxVMConfig {
    AxVMConfig::new(AxVMConfigParams {
        id: 1,
        name: "hybrid".into(),
        phys_cpu_ls: PhysCpuList::new(1, None, None),
        pass_through_devices: vec![PassThroughDeviceConfig {
            name: path.into(),
            ..Default::default()
        }],
        interrupt_mode: VMInterruptMode::Hybrid,
        ..Default::default()
    })
}

fn uefi_crate_config() -> AxVMCrateConfig {
    let mut config = AxVMCrateConfig::default();
    config.kernel.boot_protocol = Some(VMBootProtocol::Uefi);
    config
}

fn config_with_selection_and_exclusion(selection: &str, excluded: &str) -> AxVMConfig {
    AxVMConfig::new(AxVMConfigParams {
        id: 1,
        name: "hybrid".into(),
        phys_cpu_ls: PhysCpuList::new(1, None, None),
        pass_through_devices: vec![PassThroughDeviceConfig {
            name: selection.into(),
            ..Default::default()
        }],
        excluded_devices: vec![vec![excluded.into()]],
        ..Default::default()
    })
}

fn interrupt_device(name: &str, interrupt_parent: u32, spi_offset: u32) -> Node {
    let mut node = Node::new(name);
    node.add_property(cells_property("interrupt-parent", &[interrupt_parent]));
    node.add_property(cells_property("interrupts", &[0, spi_offset, 4]));
    node
}

fn cells_property(name: &str, cells: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(cells);
    property
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}
