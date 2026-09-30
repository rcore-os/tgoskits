use std::string::ToString;

use super::*;

fn fdt_identity(snapshot: &HostSerialSnapshot) -> &GuestSerialFdtIdentity {
    let GuestSerialFirmwareIdentity::Fdt(identity) = &snapshot.identity else {
        panic!("FDT serial probe returned a non-FDT identity");
    };
    identity
}

fn tree_with_controller(compatible: &str, name: &str) -> FdtTree {
    let mut tree = FdtTree::new();
    let root = tree.inner().root_id();
    tree.set_property(root, prop_u32("#address-cells", 2))
        .unwrap();
    tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
    tree.set_property(root, prop_u32("interrupt-parent", 7))
        .unwrap();
    let controller = tree.add_node(root, Node::new(name));
    tree.set_property(controller, prop_string("compatible", compatible))
        .unwrap();
    tree.set_property(controller, Property::new("interrupt-controller", vec![]))
        .unwrap();
    tree.set_property(
        controller,
        prop_u32(
            "#interrupt-cells",
            if compatible.contains("gic") { 3 } else { 1 },
        ),
    )
    .unwrap();
    tree.set_property(controller, prop_u32("phandle", 7))
        .unwrap();
    tree
}

#[test]
fn installs_pl011_with_gic_spi_and_stdout_path() {
    let mut tree = tree_with_controller("arm,gic-v3", "intc@8000000");
    let profile = GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };

    install_mmio_serial(
        &mut tree,
        profile,
        GuestSerialFdtInterrupt::GicSpi,
        None,
        true,
    )
    .unwrap();
    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();
    let serial = fdt.get_by_path("/pl011@9000000").unwrap();
    let regs = serial.regs();

    assert!(
        serial
            .as_node()
            .compatibles()
            .any(|value| value == "arm,pl011")
    );
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].address, 0x0900_0000);
    assert_eq!(regs[0].size, Some(0x1000));
    assert_eq!(
        serial
            .as_node()
            .get_property("clock-frequency")
            .unwrap()
            .get_u32(),
        Some(24_000_000)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("current-speed")
            .unwrap()
            .get_u32(),
        Some(115_200)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 1, 4]
    );
    let clock = fdt.get_by_path("/vuart-clock").unwrap();
    assert!(
        clock
            .as_node()
            .compatibles()
            .any(|value| value == "fixed-clock")
    );
    assert_eq!(
        clock
            .as_node()
            .get_property("#clock-cells")
            .unwrap()
            .get_u32(),
        Some(0)
    );
    assert_eq!(
        clock
            .as_node()
            .get_property("clock-frequency")
            .unwrap()
            .get_u32(),
        Some(24_000_000)
    );
    let clock_phandle = clock
        .as_node()
        .get_property("phandle")
        .unwrap()
        .get_u32()
        .unwrap();
    assert_eq!(
        serial
            .as_node()
            .get_property("clocks")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [clock_phandle, clock_phandle]
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("clock-names")
            .unwrap()
            .as_str_iter()
            .collect::<Vec<_>>(),
        ["uartclk", "apb_pclk"]
    );
    assert_eq!(
        fdt.get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("stdout-path")
            .unwrap()
            .as_str(),
        Some("/pl011@9000000")
    );
}

#[test]
fn preserves_selected_physical_serial_and_removes_unselected_serials() {
    let mut tree = tree_with_controller("arm,gic-v3", "intc@8000000");
    let root = tree.inner().root_id();
    for (name, base) in [
        ("serial@feb50000", 0xfeb5_0000),
        ("serial@feb80000", 0xfeb8_0000),
        ("serial@feb90000", 0xfeb9_0000),
    ] {
        let serial = tree.add_node(root, Node::new(name));
        tree.set_property(serial, prop_string("compatible", "ns16550a"))
            .unwrap();
        tree.inner_mut()
            .view_typed_mut(serial)
            .unwrap()
            .set_regs(&[RegInfo::new(base, Some(0x100))]);
    }
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(
        chosen,
        prop_string("stdout-path", "/serial@feb50000:1500000"),
    )
    .unwrap();
    let profile = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };

    install_mmio_serial_preserving(
        &mut tree,
        profile,
        GuestSerialFdtInterrupt::GicSpi,
        None,
        true,
        &["/serial@feb90000".to_string()],
    )
    .unwrap();
    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();

    assert!(fdt.get_by_path_id("/serial@feb50000").is_none());
    assert!(fdt.get_by_path_id("/serial@feb80000").is_none());
    assert!(fdt.get_by_path_id("/serial@feb90000").is_some());
    assert!(fdt.get_by_path_id("/serial@9000000").is_some());
    assert_eq!(
        fdt.get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("stdout-path")
            .unwrap()
            .as_str(),
        Some("/serial@9000000")
    );
}

#[test]
fn preserves_physical_serial_selected_through_parent_path() {
    let mut tree = tree_with_controller("arm,gic-v3", "intc@8000000");
    let root = tree.inner().root_id();
    let console = tree.add_node(root, Node::new("serial@feb50000"));
    let peripherals = tree.add_node(root, Node::new("peripherals"));
    let lookalike = tree.add_node(root, Node::new("peripherals-extra"));
    for bus in [peripherals, lookalike] {
        tree.set_property(bus, prop_u32("#address-cells", 2))
            .unwrap();
        tree.set_property(bus, prop_u32("#size-cells", 2)).unwrap();
    }
    let selected = tree.add_node(peripherals, Node::new("serial@feb90000"));
    let unselected = tree.add_node(lookalike, Node::new("serial@feba0000"));
    for (serial, base) in [
        (console, 0xfeb5_0000),
        (selected, 0xfeb9_0000),
        (unselected, 0xfeba_0000),
    ] {
        tree.set_property(serial, prop_string("compatible", "ns16550a"))
            .unwrap();
        tree.inner_mut()
            .view_typed_mut(serial)
            .unwrap()
            .set_regs(&[RegInfo::new(base, Some(0x100))]);
    }
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(
        chosen,
        prop_string("stdout-path", "/serial@feb50000:1500000"),
    )
    .unwrap();
    let profile = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };

    install_mmio_serial_preserving(
        &mut tree,
        profile,
        GuestSerialFdtInterrupt::GicSpi,
        None,
        true,
        &["/peripherals".to_string()],
    )
    .unwrap();
    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();

    assert!(fdt.get_by_path_id("/serial@feb50000").is_none());
    assert!(fdt.get_by_path_id("/peripherals/serial@feb90000").is_some());
    assert!(
        fdt.get_by_path_id("/peripherals-extra/serial@feba0000")
            .is_none()
    );
    assert!(fdt.get_by_path_id("/serial@9000000").is_some());
}

#[test]
fn installs_ns16550a_with_plic_source() {
    let mut tree = tree_with_controller("riscv,plic0", "plic@c000000");
    let profile = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x1000_0000,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 10,
        clock_hz: 3_686_400,
    };

    install_mmio_serial(
        &mut tree,
        profile,
        GuestSerialFdtInterrupt::PlicSource,
        None,
        true,
    )
    .unwrap();
    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();
    let serial = fdt.get_by_path("/serial@10000000").unwrap();
    let regs = serial.regs();

    assert!(
        serial
            .as_node()
            .compatibles()
            .any(|value| value == "ns16550a")
    );
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].address, 0x1000_0000);
    assert_eq!(regs[0].size, Some(0x100));
    assert_eq!(
        serial
            .as_node()
            .get_property("reg-shift")
            .unwrap()
            .get_u32(),
        Some(0)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("reg-io-width")
            .unwrap()
            .get_u32(),
        Some(1)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("clock-frequency")
            .unwrap()
            .get_u32(),
        Some(3_686_400)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("current-speed")
            .unwrap()
            .get_u32(),
        Some(115_200)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [10]
    );
}

#[test]
fn replaces_host_serial_nodes_and_console_aliases() {
    let mut tree = tree_with_controller("riscv,plic0", "plic@c000000");
    let soc = tree.ensure_path("/soc").unwrap();
    let old_uart = tree.add_node(soc, Node::new("uart@1000"));
    tree.set_property(old_uart, prop_string("compatible", "ns16550a"))
        .unwrap();
    let old_pl011 = tree.add_node(tree.inner().root_id(), Node::new("debug@2000"));
    tree.set_property(old_pl011, prop_string("compatible", "arm,pl011"))
        .unwrap();
    let aliases = tree.ensure_path("/aliases").unwrap();
    tree.set_property(aliases, prop_string("uart0", "/soc/uart@1000"))
        .unwrap();
    tree.set_property(aliases, prop_string("serial0", "/debug@2000"))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_string("stdout-path", "uart0:115200n8"))
        .unwrap();

    let profile = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x1000_0000,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 10,
        clock_hz: 3_686_400,
    };
    install_mmio_serial(
        &mut tree,
        profile,
        GuestSerialFdtInterrupt::PlicSource,
        None,
        true,
    )
    .unwrap();

    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();
    assert!(fdt.get_by_path("/soc/uart@1000").is_none());
    assert!(fdt.get_by_path("/debug@2000").is_none());
    assert!(fdt.get_by_path("/serial@10000000").is_some());
    assert_eq!(
        fdt.get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("serial0")
            .unwrap()
            .as_str(),
        Some("/serial@10000000")
    );
    assert_eq!(
        fdt.get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("stdout-path")
            .unwrap()
            .as_str(),
        Some("/serial@10000000")
    );
}

#[test]
fn installs_pl011_with_host_irq_phandle_and_stdout_identity() {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@fe600000");
    let root = tree.inner().root_id();
    let host_serial = tree.add_node(root, Node::new("serial@feb50000"));
    tree.set_property(
        host_serial,
        prop_string_list("compatible", &["arm,pl011", "arm,primecell"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(host_serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x1000))]);
    tree.set_property(host_serial, prop_u32_list("interrupts", &[0, 0x14d, 4]))
        .unwrap();
    tree.set_property(host_serial, prop_u32("phandle", 0x2d1))
        .unwrap();
    let aliases = tree.ensure_path("/aliases").unwrap();
    tree.set_property(aliases, prop_string("serial2", "/serial@feb50000"))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_string("stdout-path", "serial2:1500000"))
        .unwrap();

    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();
    let fallback = GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };
    let resolved = host_selected_serial(&host_fdt, fallback, GuestSerialFdtInterrupt::GicSpi)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.profile.model, GuestSerialModel::Pl011);

    let mut tree = FdtTree::from_bytes(&host_dtb).unwrap();
    install_mmio_serial(
        &mut tree,
        resolved.profile,
        GuestSerialFdtInterrupt::GicSpi,
        Some(fdt_identity(&resolved)),
        true,
    )
    .unwrap();
    let fdt = Fdt::from_bytes(&tree.finish()).unwrap();
    let serial = fdt.get_by_path("/serial@feb50000").unwrap();

    assert!(
        serial
            .as_node()
            .compatibles()
            .any(|value| value == "arm,pl011")
    );
    assert!(serial.as_node().get_property("reg-shift").is_none());
    assert!(serial.as_node().get_property("reg-io-width").is_none());
    assert_eq!(serial.regs()[0].address, 0xfeb5_0000);
    assert_eq!(serial.regs()[0].size, Some(0x1000));
    assert_eq!(
        serial.as_node().get_property("phandle").unwrap().get_u32(),
        Some(0x2d1)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("linux,phandle")
            .unwrap()
            .get_u32(),
        Some(0x2d1)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("interrupt-parent")
            .unwrap()
            .get_u32(),
        Some(7)
    );
    assert_eq!(
        serial
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 0x14d, 4]
    );
    assert_eq!(
        fdt.get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("serial2")
            .unwrap()
            .as_str(),
        Some("/serial@feb50000")
    );
    assert_eq!(
        fdt.get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("stdout-path")
            .unwrap()
            .as_str(),
        Some("serial2:1500000")
    );
}

#[test]
fn resolves_dw_apb_uart_as_virtual_16550() {
    let mut tree = FdtTree::new();
    let root = tree.inner().root_id();
    tree.set_property(root, prop_u32("#address-cells", 2))
        .unwrap();
    tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
    tree.set_property(root, prop_u32("interrupt-parent", 1))
        .unwrap();
    let gic = tree.add_node(root, Node::new("interrupt-controller@fe600000"));
    tree.set_property(gic, prop_string("compatible", "arm,gic-v3"))
        .unwrap();
    tree.set_property(gic, Property::new("interrupt-controller", vec![]))
        .unwrap();
    tree.set_property(gic, prop_u32("#interrupt-cells", 3))
        .unwrap();
    tree.set_property(gic, prop_u32("phandle", 1)).unwrap();
    let cru = tree.add_node(root, Node::new("clock-controller@fd7c0000"));
    tree.set_property(cru, prop_string("compatible", "rockchip,rk3588-cru"))
        .unwrap();
    tree.inner_mut()
        .view_typed_mut(cru)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfd7c_0000, Some(0x5c000))]);
    tree.set_property(cru, prop_u32("#clock-cells", 1)).unwrap();
    tree.set_property(cru, prop_u32("phandle", 2)).unwrap();
    let serial = tree.add_node(root, Node::new("serial@feb50000"));
    tree.set_property(
        serial,
        prop_string_list("compatible", &["rockchip,rk3588-uart", "snps,dw-apb-uart"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x100))]);
    tree.set_property(serial, prop_u32("reg-shift", 2)).unwrap();
    tree.set_property(serial, prop_u32("reg-io-width", 4))
        .unwrap();
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 0x14d, 4]))
        .unwrap();
    tree.set_property(serial, prop_u32_list("clocks", &[2, 187, 2, 172]))
        .unwrap();
    tree.set_property(serial, prop_u32("phandle", 0x2d1))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(
        chosen,
        prop_string("stdout-path", "/serial@feb50000:1500000"),
    )
    .unwrap();

    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();
    let fallback = GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };

    let resolved = host_selected_serial(&host_fdt, fallback, GuestSerialFdtInterrupt::GicSpi)
        .unwrap()
        .unwrap();

    assert_eq!(
        resolved.profile,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0xfeb5_0000,
                length: 0x100,
                register_shift: 2,
                register_width: AccessWidth::Dword,
            },
            irq: 365,
            clock_hz: 24_000_000,
        }
    );
    let identity = fdt_identity(&resolved);
    assert_eq!(identity.node_path, "/serial@feb50000");
    assert_eq!(identity.node_phandle, Some(0x2d1));
    assert_eq!(identity.interrupt_parent, 1);
    assert_eq!(identity.interrupt_specifier, [0, 0x14d, 4]);
    assert_eq!(identity.stdout_path, "/serial@feb50000:1500000");
    assert_eq!(
        identity.clock_references,
        [
            GuestClockReference {
                provider_phandle: 2,
                specifier: vec![187],
                provider_regions: vec![GuestMmioRegion {
                    base: 0xfd7c_0000,
                    length: 0x5c000,
                }],
            },
            GuestClockReference {
                provider_phandle: 2,
                specifier: vec![172],
                provider_regions: vec![GuestMmioRegion {
                    base: 0xfd7c_0000,
                    length: 0x5c000,
                }],
            },
        ]
    );

    let mut tree = FdtTree::from_bytes(&host_dtb).unwrap();
    install_mmio_serial(
        &mut tree,
        resolved.profile,
        GuestSerialFdtInterrupt::GicSpi,
        Some(fdt_identity(&resolved)),
        true,
    )
    .unwrap();
    let guest_fdt = Fdt::from_bytes(&tree.finish()).unwrap();
    let guest_serial = guest_fdt.get_by_path("/serial@feb50000").unwrap();
    assert!(
        guest_serial
            .as_node()
            .compatibles()
            .any(|compatible| compatible == "ns16550a")
    );
    assert_eq!(
        guest_serial
            .as_node()
            .get_property("reg-shift")
            .unwrap()
            .get_u32(),
        Some(2)
    );
    assert_eq!(
        guest_serial
            .as_node()
            .get_property("reg-io-width")
            .unwrap()
            .get_u32(),
        Some(4)
    );
    assert!(guest_fdt.get_by_path("/vuart-clock").is_none());
}

#[test]
fn resolves_earlycon_uart_when_stdout_path_is_missing() {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@fd400000");
    let root = tree.inner().root_id();
    let serial = tree.add_node(root, Node::new("serial@fe660000"));
    tree.set_property(
        serial,
        prop_string_list("compatible", &["rockchip,rk3568-uart", "snps,dw-apb-uart"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfe66_0000, Some(0x100))]);
    tree.set_property(serial, prop_u32("reg-shift", 2)).unwrap();
    tree.set_property(serial, prop_u32("reg-io-width", 4))
        .unwrap();
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 0x76, 4]))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(
        chosen,
        prop_string(
            "bootargs",
            "earlycon=uart8250,mmio32,0xfe660000 console=ttyFIQ0",
        ),
    )
    .unwrap();

    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();
    let fallback = GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };

    let resolved = host_selected_serial(&host_fdt, fallback, GuestSerialFdtInterrupt::GicSpi)
        .unwrap()
        .unwrap();

    assert_eq!(
        resolved.profile,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0xfe66_0000,
                length: 0x100,
                register_shift: 2,
                register_width: AccessWidth::Dword,
            },
            irq: 150,
            clock_hz: 24_000_000,
        }
    );
    let identity = fdt_identity(&resolved);
    assert_eq!(identity.node_path, "/serial@fe660000");
    assert_eq!(identity.interrupt_specifier, [0, 0x76, 4]);
    assert_eq!(identity.stdout_path, "/serial@fe660000");
}

#[test]
fn rejects_truncated_host_serial_clock_specifier() {
    let mut tree = FdtTree::new();
    let root = tree.inner().root_id();
    tree.set_property(root, prop_u32("#address-cells", 2))
        .unwrap();
    tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
    let provider = tree.add_node(root, Node::new("clock-controller@fdd20000"));
    tree.inner_mut()
        .view_typed_mut(provider)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfdd2_0000, Some(0x1000))]);
    tree.set_property(provider, prop_u32("#clock-cells", 1))
        .unwrap();
    tree.set_property(provider, prop_u32("phandle", 0x23))
        .unwrap();
    let serial = tree.add_node(root, Node::new("serial@fe660000"));
    tree.set_property(serial, prop_u32_list("clocks", &[0x23]))
        .unwrap();

    let bytes = tree.finish();
    let fdt = Fdt::from_bytes(&bytes).unwrap();
    let serial = fdt.get_by_path("/serial@fe660000").unwrap();
    let error = serial_clock_references(&fdt, serial.as_node(), "/serial@fe660000").unwrap_err();

    assert!(matches!(error, crate::AxVmError::InvalidConfig { .. }));
    assert!(error.to_string().contains("truncated clock specifier"));
}

/// Zephyr selects its console through `/chosen` phandle properties instead of
/// `stdout-path`, so a supplied firmware DTB may omit the string selector.
const ZEPHYR_CONSOLE_PHANDLE: u32 = 0x2d1;

/// Builds a synthetic AArch64 firmware DTB whose only console selection is the
/// `/chosen` `zephyr,console`/`zephyr,shell-uart` phandle pair.
fn zephyr_phandle_console_tree() -> FdtTree {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@8000000");
    let gic = tree
        .inner()
        .get_by_path_id("/interrupt-controller@8000000")
        .unwrap();
    tree.inner_mut().view_typed_mut(gic).unwrap().set_regs(&[
        RegInfo::new(0x0800_0000, Some(0x1_0000)),
        RegInfo::new(0x080a_0000, Some(0xf6_0000)),
    ]);
    let root = tree.inner().root_id();
    let serial = tree.add_node(root, Node::new("serial@feb50000"));
    tree.set_property(
        serial,
        prop_string_list("compatible", &["rockchip,rk3588-uart", "snps,dw-apb-uart"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x100))]);
    tree.set_property(serial, prop_string("status", "okay"))
        .unwrap();
    tree.set_property(serial, prop_u32("reg-shift", 2)).unwrap();
    tree.set_property(serial, prop_u32("reg-io-width", 4))
        .unwrap();
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 0x14d, 4]))
        .unwrap();
    tree.set_property(serial, prop_u32("phandle", ZEPHYR_CONSOLE_PHANDLE))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_u32("zephyr,console", ZEPHYR_CONSOLE_PHANDLE))
        .unwrap();
    tree.set_property(
        chosen,
        prop_u32("zephyr,shell-uart", ZEPHYR_CONSOLE_PHANDLE),
    )
    .unwrap();
    tree
}

/// Rebuilds the Zephyr fixture with path-string selectors and no serial phandle,
/// matching firmware that emits `zephyr,console = "/serial@feb50000";`.
fn zephyr_path_string_console_tree() -> FdtTree {
    let mut tree = zephyr_phandle_console_tree();
    let serial = tree.inner().get_by_path_id("/serial@feb50000").unwrap();
    tree.inner_mut()
        .node_mut(serial)
        .unwrap()
        .remove_property("phandle");
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_string("zephyr,console", "/serial@feb50000"))
        .unwrap();
    tree.set_property(chosen, prop_string("zephyr,shell-uart", "/serial@feb50000"))
        .unwrap();
    tree
}

fn zephyr_console_fallback() -> GuestSerialProfile {
    GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    }
}

/// Builds a host firmware DTB whose console is selected by `stdout-path`.
fn host_pl011_console_tree() -> FdtTree {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@8000000");
    let root = tree.inner().root_id();
    let serial = tree.add_node(root, Node::new("pl011@9000000"));
    tree.set_property(
        serial,
        prop_string_list("compatible", &["arm,pl011", "arm,primecell"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0900_0000, Some(0x1000))]);
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 1, 4]))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_string("stdout-path", "/pl011@9000000"))
        .unwrap();
    tree
}

/// Builds a supplied DTB that carries a serial node but selects no console.
fn bare_serial_tree() -> FdtTree {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@8000000");
    let root = tree.inner().root_id();
    let serial = tree.add_node(root, Node::new("serial@feb50000"));
    tree.set_property(serial, prop_string("compatible", "ns16550a"))
        .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x100))]);
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 0x14d, 4]))
        .unwrap();
    tree
}

#[test]
fn supplied_console_beats_host_selected_console() {
    let provided = zephyr_phandle_console_tree().finish();
    let host = host_pl011_console_tree().finish();

    let resolved = resolve_console_source(
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
        false,
        Some(provided.as_slice()),
        Some(host.as_slice()),
    )
    .unwrap()
    .expect("the supplied console contract must take priority over the host serial");

    assert_eq!(resolved.snapshot.profile.model, GuestSerialModel::Uart16550);
    let GuestSerialTransport::Mmio { base, .. } = resolved.snapshot.profile.transport else {
        panic!("supplied console selection must use MMIO");
    };
    assert_eq!(base, 0xfeb5_0000);
    assert_eq!(
        fdt_identity(&resolved.snapshot).node_path,
        "/serial@feb50000"
    );
}

#[test]
fn explicit_console0_overrides_implicit_console_sources() {
    let provided = zephyr_phandle_console_tree().finish();
    let host = host_pl011_console_tree().finish();

    let resolved = resolve_console_source(
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
        true,
        Some(provided.as_slice()),
        Some(host.as_slice()),
    )
    .unwrap();

    assert!(
        resolved.is_none(),
        "explicit console0 must not be overwritten by supplied or host firmware"
    );
}

#[test]
fn provided_dtb_without_console_selector_falls_back_to_host_then_machine() {
    let provided = bare_serial_tree().finish();
    let host = host_pl011_console_tree().finish();

    let resolved = resolve_console_source(
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
        false,
        Some(provided.as_slice()),
        Some(host.as_slice()),
    )
    .unwrap()
    .expect("a supplied DTB without a console selector must fall back to the host serial");
    assert_eq!(resolved.snapshot.profile.model, GuestSerialModel::Pl011);
    assert_eq!(fdt_identity(&resolved.snapshot).node_path, "/pl011@9000000");

    // `dtb_load_addr` alone is not a supplied DTB: no bytes means the host then
    // machine path is followed.
    let preserved = resolve_console_source(
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
        false,
        None,
        None,
    )
    .unwrap();
    assert!(
        preserved.is_none(),
        "no firmware console source must preserve the machine profile"
    );
}

#[test]
fn normalizes_zephyr_console_interrupt_to_runtime_controller_encoding() {
    // Mirrors the Orange Pi UART2 Zephyr DT: the firmware specifier carries an
    // extra cell and a non-level flags value. The console node must adopt the
    // per-VM controller phandle and its three-cell GIC encoding without
    // rejecting the firmware form.
    let mut identity = GuestSerialFirmwareIdentity::Fdt(GuestSerialFdtIdentity {
        node_path: "/serial@feb50000".into(),
        node_phandle: Some(ZEPHYR_CONSOLE_PHANDLE),
        interrupt_parent: 1,
        interrupt_specifier: vec![0, 0x14d, 4, 0xa0],
        stdout_path: "/serial@feb50000".into(),
        clock_references: Vec::new(),
    });

    normalize_console_identity(&mut identity, GuestSerialFdtInterrupt::GicSpi, Some(0x22));

    let GuestSerialFirmwareIdentity::Fdt(identity) = identity else {
        panic!("normalization must keep the FDT identity");
    };
    assert_eq!(identity.interrupt_parent, 0x22);
    assert_eq!(identity.interrupt_specifier, [0, 0x14d, 4]);
    assert_eq!(identity.node_phandle, Some(ZEPHYR_CONSOLE_PHANDLE));
}

#[test]
fn resolves_zephyr_phandle_console_selection() {
    let tree = zephyr_phandle_console_tree();
    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();

    let resolution = host_selected_serial(
        &host_fdt,
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
    )
    .unwrap();
    assert!(
        resolution.is_some(),
        "host_selected_serial must honor /chosen zephyr,console and zephyr,shell-uart phandles \
         when stdout-path and earlycon are absent"
    );
    let resolved = resolution.unwrap();

    assert_eq!(
        resolved.profile,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0xfeb5_0000,
                length: 0x100,
                register_shift: 2,
                register_width: AccessWidth::Dword,
            },
            irq: 0x14d + 32,
            clock_hz: 24_000_000,
        }
    );
    let identity = fdt_identity(&resolved);
    assert_eq!(identity.node_path, "/serial@feb50000");
    assert_eq!(identity.node_phandle, Some(ZEPHYR_CONSOLE_PHANDLE));
    assert_eq!(identity.interrupt_parent, 7);
    assert_eq!(identity.interrupt_specifier, [0, 0x14d, 4]);
    assert!(!identity.stdout_path.is_empty());
}

#[test]
fn resolves_zephyr_path_string_console_and_preserves_its_form() {
    let tree = zephyr_path_string_console_tree();
    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();

    let resolved = host_selected_serial(
        &host_fdt,
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
    )
    .unwrap()
    .expect("a zephyr path-string console must resolve");

    assert_eq!(resolved.profile.model, GuestSerialModel::Uart16550);
    assert_eq!(fdt_identity(&resolved).node_path, "/serial@feb50000");
    assert_eq!(fdt_identity(&resolved).node_phandle, None);

    let mut guest = FdtTree::from_bytes(&host_dtb).unwrap();
    install_mmio_serial(
        &mut guest,
        resolved.profile,
        GuestSerialFdtInterrupt::GicSpi,
        Some(fdt_identity(&resolved)),
        true,
    )
    .unwrap();
    let patched = Fdt::from_bytes(&guest.finish()).unwrap();

    let chosen = patched.get_by_path("/chosen").unwrap().as_node();
    assert_eq!(
        chosen.get_property("zephyr,console").unwrap().as_str(),
        Some("/serial@feb50000")
    );
    assert_eq!(
        chosen.get_property("zephyr,shell-uart").unwrap().as_str(),
        Some("/serial@feb50000")
    );
    assert!(
        patched
            .get_by_path("/serial@feb50000")
            .unwrap()
            .as_node()
            .get_property("phandle")
            .is_none(),
        "a path-string selector must not force a serial phandle"
    );
}

#[test]
fn rejects_zephyr_console_phandle_without_matching_node() {
    let mut tree = zephyr_phandle_console_tree();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_u32("zephyr,console", 0xdead))
        .unwrap();
    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();

    let result = host_selected_serial(
        &host_fdt,
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
    );
    assert!(
        result.is_err(),
        "a dangling zephyr,console phandle must fail closed instead of being ignored"
    );
}

#[test]
fn rejects_malformed_zephyr_console_property() {
    let mut tree = zephyr_phandle_console_tree();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_string("zephyr,shell-uart", "uart2"))
        .unwrap();
    let host_dtb = tree.finish();
    let host_fdt = Fdt::from_bytes(&host_dtb).unwrap();

    let result = host_selected_serial(
        &host_fdt,
        zephyr_console_fallback(),
        GuestSerialFdtInterrupt::GicSpi,
    );
    assert!(
        result.is_err(),
        "a present but non-absolute zephyr,shell-uart path must fail closed"
    );
}
