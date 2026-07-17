use alloc::string::ToString;
use core::cell::Cell;

use rdrive::probe::{
    OnProbeError,
    pci::{PciAddress, PciInfo, PciIntxRoute},
};

use super::{
    DynamicPciIrqSource, LegacyIrqRoute, legacy_line_to_irq_for_platform,
    prepare_intx_passthrough_command, resolve_intx_binding_with_resolvers,
    resolve_intx_irq_with_resolvers, select_dynamic_pci_irq_source,
    unmask_intx_passthrough_command,
};
use crate::{BindingIrq, BindingIrqSource};
#[test]
fn x86_64_legacy_line_uses_dynamic_ioapic_base() {
    assert_eq!(legacy_line_to_irq_for_platform(9, true), 0x39);
}

#[test]
fn non_x86_64_legacy_line_remains_raw_irq() {
    assert_eq!(legacy_line_to_irq_for_platform(9, false), 9);
}

#[test]
fn legacy_route_uses_swizzled_root_device_and_pin() {
    let route = LegacyIrqRoute::from_irqs(0, 8, &[40, 41, 42, 43]).unwrap();
    let info = PciInfo {
        address: PciAddress::new(0, 2, 7, 0),
        interrupt_pin: 1,
        interrupt_line: 0,
        intx_route: Some(PciIntxRoute {
            root_device: 2,
            root_function: 0,
            root_pin: 4,
        }),
    };

    assert_eq!(route.irq_for(info), Some(41));
}

#[test]
fn legacy_route_ignores_endpoints_without_intx_route() {
    let route = LegacyIrqRoute::from_irqs(0, 8, &[40, 41, 42, 43]).unwrap();
    let info = PciInfo {
        address: PciAddress::new(0, 2, 7, 0),
        interrupt_pin: 1,
        interrupt_line: 0,
        intx_route: None,
    };

    assert_eq!(route.irq_for(info), None);
}

#[test]
fn resolve_intx_irq_source_prefers_acpi_when_both_backends_exist() {
    assert_eq!(
        select_dynamic_pci_irq_source(true, true),
        Some(DynamicPciIrqSource::Acpi)
    );
}

#[test]
fn resolve_intx_irq_acpi_error_does_not_fallback_to_fdt_or_legacy() {
    let info = endpoint_with_intx_route();
    let fdt_called = Cell::new(false);
    let legacy_called = Cell::new(false);
    let line_called = Cell::new(false);

    let err = resolve_intx_irq_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Acpi),
        |_| Err(OnProbeError::other("acpi irq failed")),
        |_| {
            fdt_called.set(true);
            Ok(Some(55))
        },
        |_| None,
        |_| {
            legacy_called.set(true);
            Some(66)
        },
        |_| {
            line_called.set(true);
            Some(77)
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("acpi irq failed"));
    assert!(!fdt_called.get());
    assert!(!legacy_called.get());
    assert!(!line_called.get());
}

#[test]
fn resolve_intx_binding_acpi_keeps_gsi_source_native() {
    let info = endpoint_with_intx_route();
    let irq = resolve_intx_binding_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Acpi),
        |_| Ok(Some(BindingIrq::acpi_gsi(18))),
        |_| Ok(Some(BindingIrq::acpi_gsi(19))),
        |_| None,
        |_| Some(66),
        |_| Some(77),
    )
    .unwrap()
    .unwrap();

    assert_eq!(irq.legacy_num(), None);
    assert_eq!(
        irq.as_irq_source(),
        Some(irq_framework::IrqSource::AcpiGsi(18))
    );
}

#[test]
fn resolve_intx_binding_acpi_keeps_route_metadata_native() {
    let info = endpoint_with_intx_route();
    let controller = rdrive::DeviceId::new();
    let route = irq_framework::AcpiGsiRoute {
        gsi: 10,
        controller: irq_framework::AcpiGsiController::IoApic,
        controller_id: 0,
        controller_address: 0xfec0_0000,
        controller_input: 10,
        trigger: irq_framework::AcpiIrqTrigger::Level,
        polarity: irq_framework::AcpiIrqPolarity::ActiveLow,
    };
    let irq = resolve_intx_binding_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Acpi),
        |_| Ok(Some(BindingIrq::acpi_gsi_route(route))),
        |_| {
            Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                controller,
                [0, 42, 4],
            )))
        },
        |_| None,
        |_| Some(66),
        |_| Some(77),
    )
    .unwrap()
    .unwrap();

    assert_eq!(irq.legacy_num(), None);
    assert_eq!(
        irq.as_irq_source(),
        Some(irq_framework::IrqSource::AcpiGsiRoute(route))
    );
}

#[test]
fn resolve_intx_binding_fdt_keeps_interrupt_cells_native() {
    let info = endpoint_with_intx_route();
    let controller = rdrive::DeviceId::new();
    let irq = resolve_intx_binding_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Fdt),
        |_| Ok(Some(BindingIrq::acpi_gsi(18))),
        |_| {
            Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                controller,
                [0, 42, 4],
            )))
        },
        |_| None,
        |_| Some(66),
        |_| Some(77),
    )
    .unwrap()
    .unwrap();

    assert_eq!(irq.legacy_num(), None);
    let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) = irq else {
        panic!("expected native FDT interrupt binding");
    };
    assert_eq!(spec.controller, controller);
    assert_eq!(spec.cells, [0, 42, 4]);
}

#[test]
fn resolve_intx_binding_fdt_prefers_registered_native_legacy_route() {
    let info = endpoint_with_intx_route();
    let fdt_called = Cell::new(false);
    let legacy_called = Cell::new(false);
    let line_called = Cell::new(false);
    let controller = rdrive::DeviceId::new();
    let irq = resolve_intx_binding_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Fdt),
        |_| Ok(Some(BindingIrq::acpi_gsi(18))),
        |_| {
            fdt_called.set(true);
            Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                controller,
                [0, 0, 4],
            )))
        },
        |_| {
            Some(BindingIrq::fdt_interrupt_with_controller(
                controller,
                [0, 245, 4],
            ))
        },
        |_| {
            legacy_called.set(true);
            Some(66)
        },
        |_| {
            line_called.set(true);
            Some(77)
        },
    )
    .unwrap()
    .unwrap();

    assert!(!fdt_called.get());
    assert!(!legacy_called.get());
    assert!(!line_called.get());
    assert_eq!(irq.legacy_num(), None);
    let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) = irq else {
        panic!("expected native FDT interrupt binding");
    };
    assert_eq!(spec.controller, controller);
    assert_eq!(spec.cells, [0, 245, 4]);
}

#[test]
fn resolve_intx_binding_without_dynamic_firmware_uses_legacy_irq_line() {
    let info = endpoint_with_intx_route();
    let controller = rdrive::DeviceId::new();
    let irq = resolve_intx_binding_with_resolvers(
        info,
        None,
        |_| Ok(Some(BindingIrq::acpi_gsi(18))),
        |_| {
            Ok(Some(BindingIrq::fdt_interrupt_with_controller(
                controller,
                [0, 42, 4],
            )))
        },
        |_| None,
        |_| None,
        |_| Some(77),
    )
    .unwrap()
    .unwrap();

    assert_eq!(irq.legacy_num(), Some(77));
    assert_eq!(irq.as_irq_source(), None);
}

#[test]
fn resolve_intx_irq_fdt_none_does_not_fallback_to_legacy_or_interrupt_line() {
    let info = endpoint_with_intx_route();
    let acpi_called = Cell::new(false);
    let legacy_called = Cell::new(false);
    let line_called = Cell::new(false);

    let irq = resolve_intx_irq_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Fdt),
        |_| {
            acpi_called.set(true);
            Ok(Some(44))
        },
        |_| Ok(None),
        |_| None,
        |_| {
            legacy_called.set(true);
            Some(66)
        },
        |_| {
            line_called.set(true);
            Some(77)
        },
    )
    .unwrap();

    assert_eq!(irq, None);
    assert!(!acpi_called.get());
    assert!(!legacy_called.get());
    assert!(!line_called.get());
}

#[test]
fn resolve_intx_irq_acpi_none_does_not_fallback_to_legacy_or_interrupt_line() {
    let info = endpoint_with_intx_route();
    let fdt_called = Cell::new(false);
    let legacy_called = Cell::new(false);
    let line_called = Cell::new(false);

    let irq = resolve_intx_irq_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Acpi),
        |_| Ok(None),
        |_| {
            fdt_called.set(true);
            Ok(Some(55))
        },
        |_| None,
        |_| {
            legacy_called.set(true);
            Some(66)
        },
        |_| {
            line_called.set(true);
            Some(77)
        },
    )
    .unwrap();

    assert_eq!(irq, None);
    assert!(!fdt_called.get());
    assert!(!legacy_called.get());
    assert!(!line_called.get());
}

#[test]
fn resolve_intx_irq_dynamic_source_without_intx_route_does_not_use_interrupt_line() {
    let info = PciInfo {
        intx_route: None,
        ..endpoint_with_intx_route()
    };
    let acpi_called = Cell::new(false);
    let fdt_called = Cell::new(false);
    let legacy_called = Cell::new(false);
    let line_called = Cell::new(false);

    let irq = resolve_intx_irq_with_resolvers(
        info,
        Some(DynamicPciIrqSource::Acpi),
        |_| {
            acpi_called.set(true);
            Ok(Some(44))
        },
        |_| {
            fdt_called.set(true);
            Ok(Some(55))
        },
        |_| None,
        |_| {
            legacy_called.set(true);
            Some(66)
        },
        |_| {
            line_called.set(true);
            Some(77)
        },
    )
    .unwrap();

    assert_eq!(irq, None);
    assert!(!acpi_called.get());
    assert!(!fdt_called.get());
    assert!(!legacy_called.get());
    assert!(!line_called.get());
}

#[test]
fn resolve_intx_irq_static_source_keeps_legacy_and_interrupt_line_fallback() {
    let info = endpoint_with_intx_route();
    let acpi_called = Cell::new(false);
    let fdt_called = Cell::new(false);
    let line_called = Cell::new(false);

    let irq = resolve_intx_irq_with_resolvers(
        info,
        None,
        |_| {
            acpi_called.set(true);
            Ok(Some(44))
        },
        |_| {
            fdt_called.set(true);
            Ok(Some(55))
        },
        |_| None,
        |_| None,
        |line| {
            line_called.set(true);
            assert_eq!(line, 9);
            Some(77)
        },
    )
    .unwrap();

    assert_eq!(irq, Some(77));
    assert!(!acpi_called.get());
    assert!(!fdt_called.get());
    assert!(line_called.get());
}

#[test]
fn prepare_intx_passthrough_command_masks_native_intx_until_guest_route_ready() {
    let mut command = pcie::CommandRegister::INTERRUPT_DISABLE;

    command = prepare_intx_passthrough_command(command);

    assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
    assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
    assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
    assert!(command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));

    command = prepare_intx_passthrough_command(pcie::CommandRegister::empty());

    assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
    assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
    assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
    assert!(command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));
}

#[test]
fn unmask_intx_passthrough_command_clears_native_intx_mask() {
    let mut command = pcie::CommandRegister::INTERRUPT_DISABLE
        | pcie::CommandRegister::IO_ENABLE
        | pcie::CommandRegister::MEMORY_ENABLE
        | pcie::CommandRegister::BUS_MASTER_ENABLE;

    command = unmask_intx_passthrough_command(command);

    assert!(command.contains(pcie::CommandRegister::IO_ENABLE));
    assert!(command.contains(pcie::CommandRegister::MEMORY_ENABLE));
    assert!(command.contains(pcie::CommandRegister::BUS_MASTER_ENABLE));
    assert!(!command.contains(pcie::CommandRegister::INTERRUPT_DISABLE));
}

fn endpoint_with_intx_route() -> PciInfo {
    PciInfo {
        address: PciAddress::new(0, 2, 7, 0),
        interrupt_pin: 1,
        interrupt_line: 9,
        intx_route: Some(PciIntxRoute {
            root_device: 2,
            root_function: 0,
            root_pin: 1,
        }),
    }
}
