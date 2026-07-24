use axtest::prelude::*;
use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, HwIrq, IrqDomainId, IrqId,
    IrqSource,
};
use rdrive::{DeviceId, ProbeError, error::DriverError, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, BindingIrqSource, Error, FdtIrqSpec, binding_info_from_acpi_route,
};

fn route() -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: 33,
        vector: 44,
        controller: AcpiGsiController::IoApic,
        controller_id: 1,
        controller_address: 0xfec0_0000,
        controller_input: 5,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

#[axtest]
fn ax_driver_binding_info_handles_empty_legacy_and_explicit_irq_ids() {
    let empty = BindingInfo::empty();
    ax_assert!(empty.irq().is_none());
    ax_assert_eq!(empty.irq_num(), None);
    ax_assert!(empty.irq_sources().is_empty());

    let legacy = BindingInfo::with_irq(Some(5)).unwrap();
    ax_assert_eq!(legacy.irq_num(), Some(5));
    ax_assert_eq!(legacy.irq_num_for_source(0), Some(5));
    ax_assert!(legacy.irq_cloned().unwrap().irq_id().is_some());

    let id = IrqId::new(IrqDomainId(7), HwIrq(9));
    let explicit = BindingInfo::with_irq_id(Some(id));
    ax_assert_eq!(explicit.irq_cloned(), Some(BindingIrq::id(id)));
    ax_assert_eq!(explicit.irq_for_source_cloned(0), Some(BindingIrq::id(id)));
    ax_assert_eq!(explicit.irq_num(), None);
}

#[axtest]
fn ax_driver_binding_info_tracks_multiple_named_irq_sources() {
    let first = BindingIrq::acpi_gsi(33);
    let second = BindingIrq::acpi_gsi_route(route());
    let info = BindingInfo::with_irq_sources([(1, first.clone()), (2, second.clone())]);

    ax_assert_eq!(info.irq_sources().len(), 2);
    ax_assert_eq!(info.irq_for_source(1), Some(&first));
    ax_assert_eq!(info.irq_for_source_cloned(2), Some(second));
    ax_assert_eq!(info.irq(), Some(&first));
    ax_assert_eq!(info.irq_for_source(99), None);
}

#[axtest]
fn ax_driver_binding_irq_sources_convert_to_framework_sources() {
    let gsi = BindingIrqSource::acpi_gsi(19);
    ax_assert_eq!(gsi.as_irq_source(), Some(IrqSource::AcpiGsi(19)));

    let route_source = BindingIrqSource::acpi_gsi_route(route());
    ax_assert_eq!(
        route_source.as_irq_source(),
        Some(IrqSource::AcpiGsiRoute(route()))
    );

    let controller = DeviceId::from(7);
    let fdt = BindingIrqSource::fdt_interrupt_with_controller(controller, alloc::vec![1, 2, 3]);
    ax_assert_eq!(fdt.as_irq_source(), None);
    ax_assert_eq!(
        BindingIrq::fdt_interrupt_with_controller(controller, alloc::vec![4, 5]),
        BindingIrq::Source(BindingIrqSource::FdtInterrupt(FdtIrqSpec {
            controller,
            cells: alloc::vec![4, 5],
        }))
    );
}

#[axtest]
fn ax_driver_converts_rdif_intc_acpi_routes_without_losing_metadata() {
    let rdif_route = rdif_intc::AcpiGsiRoute {
        gsi: 77,
        vector: 88,
        controller: rdif_intc::AcpiGsiController::PchPic,
        controller_id: 2,
        controller_address: 0x1000,
        controller_input: 9,
        trigger: rdif_intc::AcpiIrqTrigger::Edge,
        polarity: rdif_intc::AcpiIrqPolarity::ActiveHigh,
    };

    let source = BindingIrqSource::from(rdif_route);
    let converted = match source.as_irq_source().unwrap() {
        IrqSource::AcpiGsiRoute(route) => route,
        _ => panic!("expected route source"),
    };
    ax_assert_eq!(converted.gsi, 77);
    ax_assert_eq!(converted.controller, AcpiGsiController::PchPic);
    ax_assert_eq!(converted.trigger, AcpiIrqTrigger::Edge);
    ax_assert_eq!(converted.polarity, AcpiIrqPolarity::ActiveHigh);

    let info = binding_info_from_acpi_route("mock", Some(rdif_route)).unwrap();
    ax_assert_eq!(info.irq_cloned(), Some(BindingIrq::Source(source)));
}

#[axtest]
fn ax_driver_error_conversions_preserve_driver_and_probe_categories() {
    let driver_error = Error::from(DriverError::Unsupported("mock"));
    ax_assert!(matches!(driver_error, Error::Driver(_)));
    ax_assert!(alloc::format!("{driver_error}").contains("driver init failed"));

    let probe_error = Error::from(ProbeError::Unsupported("mock-probe"));
    ax_assert!(matches!(probe_error, Error::Probe(_)));
    ax_assert!(alloc::format!("{probe_error}").contains("driver probe failed"));

    let on_probe = Error::from(ProbeError::from(OnProbeError::NotMatch));
    ax_assert!(matches!(on_probe, Error::Probe(_)));
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn ax_driver_cmos_register_constants_hold() {
    ax_assert!(crate::time::cmos_register_constants_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn ax_driver_cmos_io_struct_and_constants_hold() {
    ax_assert!(crate::time::cmos_io_struct_and_constants_hold_for_test());
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn ax_driver_cmos_register_edge_cases_hold() {
    ax_assert!(crate::time::cmos_register_edge_cases_hold_for_test());
}
