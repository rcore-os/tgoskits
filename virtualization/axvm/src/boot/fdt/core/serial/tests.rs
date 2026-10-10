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
fn installs_pl011_with_guest_irq_parent_and_host_stdout_identity() {
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

    for guest_phandle in [Some(19), None] {
        let mut tree = FdtTree::from_bytes(&host_dtb).unwrap();
        // The explicit guest firmware uses a different controller identity.
        let controller = tree
            .inner()
            .get_by_path_id("/interrupt-controller@fe600000")
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", 0x2d1))
            .unwrap();
        tree.set_property(controller, prop_u32("linux,phandle", 0x2d1))
            .unwrap();
        let uart = tree.inner().get_by_path_id("/serial@feb50000").unwrap();
        tree.inner_mut()
            .node_mut(uart)
            .unwrap()
            .remove_property("phandle");
        if let Some(phandle) = guest_phandle {
            tree.set_property(uart, prop_u32("phandle", phandle))
                .unwrap();
            let chosen = tree.ensure_path("/chosen").unwrap();
            tree.set_property(chosen, prop_u32("zephyr,console", phandle))
                .unwrap();
        }
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

        let phandle = serial
            .as_node()
            .get_property("phandle")
            .unwrap()
            .get_u32()
            .unwrap();
        assert_ne!(phandle, 0x2d1, "UART must not reuse the guest GIC phandle");
        if let Some(expected) = guest_phandle {
            assert_eq!(phandle, expected);
            assert_eq!(
                fdt.get_by_path("/chosen")
                    .unwrap()
                    .as_node()
                    .get_property("zephyr,console")
                    .unwrap()
                    .get_u32(),
                Some(phandle)
            );
        }
        assert_eq!(
            fdt.get_by_phandle(phandle.into()).unwrap().id(),
            serial.id()
        );
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
            serial
                .as_node()
                .get_property("linux,phandle")
                .unwrap()
                .get_u32(),
            serial
                .as_node()
                .get_property("phandle")
                .and_then(Property::get_u32)
        );
        assert_eq!(
            serial
                .as_node()
                .get_property("interrupt-parent")
                .unwrap()
                .get_u32(),
            Some(0x2d1)
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
fn explicit_guest_uart_owns_virtual_irq_and_firmware_identity() {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@fe600000");
    let root = tree.inner().root_id();
    let controller = tree
        .inner()
        .get_by_path_id("/interrupt-controller@fe600000")
        .unwrap();
    tree.set_property(controller, prop_u32("#interrupt-cells", 4))
        .unwrap();
    let serial = tree.add_node(root, Node::new("serial@feb50000"));
    tree.set_property(
        serial,
        prop_string_list("compatible", &["rockchip,rk3588-uart", "ns16550"]),
    )
    .unwrap();
    tree.inner_mut()
        .view_typed_mut(serial)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x100))]);
    tree.set_property(serial, prop_u32("reg-shift", 2)).unwrap();
    tree.set_property(serial, prop_u32_list("interrupts", &[0, 0x14d, 2, 0xa0]))
        .unwrap();
    tree.set_property(serial, prop_u32("phandle", 0x2d1))
        .unwrap();
    let chosen = tree.ensure_path("/chosen").unwrap();
    tree.set_property(chosen, prop_u32("zephyr,console", 0x2d1))
        .unwrap();
    let bytes = tree.finish();
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

    let selected = select_guest_serial(
        fallback,
        Some(&bytes),
        false,
        GuestSerialFdtInterrupt::GicSpi,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.profile.irq, 0x14d + 32);
    assert_eq!(selected.identity.node_path, "/serial@feb50000");
    assert_eq!(selected.identity.node_phandle, Some(0x2d1));
    assert_eq!(selected.identity.interrupt_parent, 7);
    assert_eq!(selected.identity.interrupt_specifier, [0, 0x14d, 2, 0xa0]);

    let mut final_tree = FdtTree::from_bytes(&bytes).unwrap();
    let gic = crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Aarch64, 1)
        .gic
        .unwrap();
    super::super::interrupt::install_machine_interrupt_controller(
        &mut final_tree,
        1,
        Some(&gic),
        None,
    )
    .unwrap();
    install_mmio_serial(
        &mut final_tree,
        selected.profile,
        GuestSerialFdtInterrupt::GicSpi,
        Some(&selected.identity),
        true,
    )
    .unwrap();
    let final_fdt = Fdt::from_bytes(&final_tree.finish()).unwrap();
    let final_controller = final_fdt
        .get_by_path("/interrupt-controller@fe600000")
        .unwrap();
    assert_eq!(
        final_controller
            .as_node()
            .get_property("#interrupt-cells")
            .unwrap()
            .get_u32(),
        Some(4)
    );
    let final_serial = final_fdt.get_by_path("/serial@feb50000").unwrap();
    assert_eq!(
        final_serial
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 0x14d, 4, 0xa0]
    );
    assert_eq!(
        final_fdt
            .get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("zephyr,console")
            .unwrap()
            .get_u32(),
        Some(0x2d1)
    );

    assert!(
        select_guest_serial(
            fallback,
            Some(&bytes),
            true,
            GuestSerialFdtInterrupt::GicSpi,
        )
        .unwrap()
        .is_none()
    );

    let mut linux_tree = FdtTree::from_bytes(&bytes).unwrap();
    let linux_controller = linux_tree
        .inner()
        .get_by_path_id("/interrupt-controller@fe600000")
        .unwrap();
    linux_tree
        .set_property(linux_controller, prop_u32("#interrupt-cells", 3))
        .unwrap();
    let linux_serial = linux_tree
        .inner()
        .get_by_path_id("/serial@feb50000")
        .unwrap();
    linux_tree
        .set_property(linux_serial, prop_u32_list("interrupts", &[0, 0x14d, 2]))
        .unwrap();
    let linux_chosen = linux_tree.ensure_path("/chosen").unwrap();
    linux_tree
        .inner_mut()
        .node_mut(linux_chosen)
        .unwrap()
        .remove_property("zephyr,console");
    linux_tree
        .set_property(linux_chosen, prop_string("stdout-path", "/serial@feb50000"))
        .unwrap();
    let linux_bytes = linux_tree.finish();
    let linux_selected = select_guest_serial(
        fallback,
        Some(&linux_bytes),
        false,
        GuestSerialFdtInterrupt::GicSpi,
    )
    .unwrap()
    .unwrap();
    assert_eq!(linux_selected.identity.interrupt_specifier, [0, 0x14d, 2]);

    let mut linux_final_tree = FdtTree::from_bytes(&linux_bytes).unwrap();
    install_mmio_serial(
        &mut linux_final_tree,
        linux_selected.profile,
        GuestSerialFdtInterrupt::GicSpi,
        Some(&linux_selected.identity),
        true,
    )
    .unwrap();
    let linux_final_bytes = linux_final_tree.finish();
    let linux_final_fdt = Fdt::from_bytes(&linux_final_bytes).unwrap();
    assert_eq!(
        linux_final_fdt
            .get_by_path("/serial@feb50000")
            .unwrap()
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 0x14d, 4]
    );

    let mut indirect_tree = FdtTree::from_bytes(&bytes).unwrap();
    let root = indirect_tree.inner().root_id();
    let secondary = indirect_tree.add_node(root, Node::new("interrupt-controller@f000000"));
    indirect_tree
        .set_property(secondary, prop_string("compatible", "arm,gic-v3"))
        .unwrap();
    indirect_tree
        .set_property(secondary, Property::new("interrupt-controller", vec![]))
        .unwrap();
    indirect_tree
        .set_property(secondary, prop_u32("#interrupt-cells", 4))
        .unwrap();
    indirect_tree
        .set_property(secondary, prop_u32("phandle", 0x44))
        .unwrap();
    let indirect_serial = indirect_tree
        .inner()
        .get_by_path_id("/serial@feb50000")
        .unwrap();
    indirect_tree
        .set_property(indirect_serial, prop_u32("interrupt-parent", 0x44))
        .unwrap();
    let indirect_bytes = indirect_tree.finish();
    let error = select_guest_serial(
        fallback,
        Some(&indirect_bytes),
        false,
        GuestSerialFdtInterrupt::GicSpi,
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("not directly connected to the selected GIC")
    );
}

#[test]
fn explicit_guest_without_console_selection_keeps_machine_uart() {
    let mut tree = tree_with_controller("arm,gic-v3", "interrupt-controller@8000000");
    let controller = tree
        .inner()
        .get_by_path_id("/interrupt-controller@8000000")
        .unwrap();
    tree.set_property(controller, prop_u32("#interrupt-cells", 4))
        .unwrap();
    let bytes = tree.finish();
    let fallback =
        crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Aarch64, 1).serial;

    assert!(
        select_guest_serial(
            fallback,
            Some(&bytes),
            false,
            GuestSerialFdtInterrupt::GicSpi,
        )
        .unwrap()
        .is_none()
    );

    let mut final_tree = FdtTree::from_bytes(&bytes).unwrap();
    install_mmio_serial(
        &mut final_tree,
        fallback,
        GuestSerialFdtInterrupt::GicSpi,
        None,
        true,
    )
    .unwrap();
    let final_fdt = Fdt::from_bytes(&final_tree.finish()).unwrap();
    assert_eq!(
        final_fdt
            .get_by_path("/pl011@9000000")
            .unwrap()
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 1, 4, 0]
    );
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
    let error =
        serial_clock_references(&fdt, serial.as_node(), "/serial@fe660000", "host").unwrap_err();

    assert!(matches!(error, crate::AxVmError::InvalidConfig { .. }));
    assert!(error.to_string().contains("truncated clock specifier"));
}
