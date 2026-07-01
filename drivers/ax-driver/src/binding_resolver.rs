use alloc::{format, vec::Vec};

use rdrive::{
    DeviceId,
    probe::{OnProbeError, acpi::AcpiInfo},
    register::FdtInfo,
};

use crate::{BindingInfo, BindingIrq};

pub fn binding_info_from_fdt(info: &FdtInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(resolve_fdt_irq(info)?))
}

#[cfg(feature = "plat-dyn")]
pub fn binding_irq_from_named_fdt_interrupt(
    node: &rdrive::probe::fdt::NodeType<'_>,
    name: &str,
) -> Result<Option<BindingIrq>, OnProbeError> {
    let interrupts = node.interrupts();
    if interrupts.is_empty() {
        return Ok(None);
    }

    let index = node
        .as_node()
        .get_property("interrupt-names")
        .and_then(|prop| prop.as_str_iter().position(|irq_name| irq_name == name))
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] interrupt-names does not contain {name}",
                node.name()
            ))
        })?;
    let interrupt = interrupts.get(index).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] interrupt-names entry {name} has no matching interrupts cell",
            node.name()
        ))
    })?;
    let controller =
        rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] interrupt-parent {} is not registered",
                node.name(),
                interrupt.interrupt_parent
            ))
        })?;

    Ok(Some(binding_irq_from_fdt_interrupt(
        controller,
        interrupt.specifier.clone(),
    )))
}

pub fn binding_info_from_acpi(info: &AcpiInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(
        info.irq_route().map(BindingIrq::from),
    ))
}

pub fn binding_info_from_acpi_route(
    _path: &str,
    route: Option<rdrive::probe::acpi::AcpiGsiRoute>,
) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(route.map(BindingIrq::from)))
}

fn resolve_fdt_irq(info: &FdtInfo<'_>) -> Result<Option<BindingIrq>, OnProbeError> {
    let Some(interrupt) = info.interrupts().into_iter().next() else {
        return Ok(None);
    };
    let controller = info
        .phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "interrupt-parent {} is not registered",
                interrupt.interrupt_parent
            ))
        })?;

    Ok(Some(binding_irq_from_fdt_interrupt(
        controller,
        interrupt.specifier,
    )))
}

fn binding_irq_from_fdt_interrupt(controller: DeviceId, cells: impl Into<Vec<u32>>) -> BindingIrq {
    BindingIrq::fdt_interrupt_with_controller(controller, cells)
}

#[cfg(feature = "pci")]
pub fn binding_info_from_pci(
    info: rdrive::probe::pci::PciInfo,
    requirement: crate::PciIrqRequirement,
) -> Result<BindingInfo, OnProbeError> {
    let irq = crate::pci::resolve_intx_binding(info)?;
    if irq.is_none() && requirement == crate::PciIrqRequirement::Required {
        return Err(OnProbeError::other(format!(
            "failed to resolve IRQ for PCI endpoint {}",
            info.address
        )));
    }
    Ok(BindingInfo::with_binding_irq(irq))
}
