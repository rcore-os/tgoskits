use alloc::format;

use rdrive::{
    probe::{OnProbeError, acpi::AcpiInfo},
    register::FdtInfo,
};

use crate::BindingInfo;

pub fn binding_info_from_fdt(info: &FdtInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_irq(resolve_fdt_irq(info)?))
}

pub fn binding_info_from_acpi(info: &AcpiInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_irq(resolve_acpi_irq(info)?))
}

pub fn binding_info_from_acpi_route(
    path: &str,
    route: Option<rdrive::probe::acpi::AcpiGsiRoute>,
) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_irq(match route {
        Some(route) => Some(setup_acpi_irq(path, &route)?),
        None => None,
    }))
}

fn resolve_fdt_irq(info: &FdtInfo<'_>) -> Result<Option<usize>, OnProbeError> {
    let Some(interrupt) = info.interrupts().into_iter().next() else {
        return Ok(None);
    };

    let interrupt_parent = info
        .phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "failed to resolve interrupt parent {:?} for {}",
                interrupt.interrupt_parent,
                info.node.path()
            ))
        })?;
    let intc = rdrive::get::<rdif_intc::Intc>(interrupt_parent).map_err(|err| {
        OnProbeError::other(format!(
            "failed to get interrupt controller {:?} for {}: {err:?}",
            interrupt_parent,
            info.node.path()
        ))
    })?;
    let mut intc = intc.lock().map_err(|err| {
        OnProbeError::other(format!(
            "failed to lock interrupt controller {:?} for {}: {err:?}",
            interrupt_parent,
            info.node.path()
        ))
    })?;
    Ok(Some(intc.setup_irq_by_fdt(&interrupt.specifier).into()))
}

fn resolve_acpi_irq(info: &AcpiInfo<'_>) -> Result<Option<usize>, OnProbeError> {
    let Some(route) = info.irq_route() else {
        return Ok(None);
    };
    setup_acpi_irq(info.path, &route).map(Some)
}

fn setup_acpi_irq(
    path: &str,
    route: &rdrive::probe::acpi::AcpiGsiRoute,
) -> Result<usize, OnProbeError> {
    let intc = rdrive::get_list::<rdif_intc::Intc>()
        .into_iter()
        .find(|intc| {
            intc.try_lock()
                .map(|intc| intc.supports_acpi_gsi(route))
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "ACPI interrupt controller for route {:?} is not registered for {path}",
                route.controller
            ))
        })?;
    let mut intc = intc.lock().map_err(|err| {
        OnProbeError::other(format!(
            "failed to lock ACPI interrupt controller for {path}: {err:?}"
        ))
    })?;
    Ok(intc.setup_irq_by_acpi(route).into())
}

#[cfg(feature = "pci")]
pub fn binding_info_from_pci(
    info: rdrive::probe::pci::PciInfo,
    requirement: crate::PciIrqRequirement,
) -> Result<BindingInfo, OnProbeError> {
    let irq = crate::pci::resolve_intx_irq(info)?;
    if irq.is_none() && requirement == crate::PciIrqRequirement::Required {
        return Err(OnProbeError::other(format!(
            "failed to resolve IRQ for PCI endpoint {}",
            info.address
        )));
    }
    Ok(BindingInfo::with_irq(irq))
}
