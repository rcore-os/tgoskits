use alloc::{format, vec::Vec};

use rdrive::{
    DeviceId,
    probe::{OnProbeError, acpi::AcpiInfo},
    register::FdtInfo,
};

use crate::{BindingInfo, BindingIrq, BindingLocator, HostMmioRange};

pub fn binding_info_from_fdt(info: &FdtInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    let irqs = resolve_fdt_irqs(info)?;
    let ranges = host_mmio_ranges_from_fdt(info)?;
    Ok(BindingInfo::with_irq_sources(irqs).with_host_resources(
        BindingLocator::Fdt {
            path: info.node.path(),
        },
        ranges,
    ))
}

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
    let ranges = host_mmio_ranges_from_acpi(info.path, info.memory_ranges())?;
    Ok(
        BindingInfo::with_binding_irq(info.irq_route().map(BindingIrq::from)).with_host_resources(
            BindingLocator::Acpi {
                path: info.path.into(),
            },
            ranges,
        ),
    )
}

pub fn binding_info_from_acpi_route(
    path: &str,
    route: Option<rdrive::probe::acpi::AcpiGsiRoute>,
) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(route.map(BindingIrq::from))
        .with_host_resources(BindingLocator::Acpi { path: path.into() }, Vec::new()))
}

#[cfg(feature = "pci")]
pub fn binding_info_from_pci(
    info: rdrive::probe::pci::PciInfo,
    requirement: crate::PciIrqRequirement,
) -> Result<BindingInfo, OnProbeError> {
    binding_info_from_pci_resources(info, requirement, Vec::new())
}

/// Resolves a PCI binding and retains every memory BAR from `endpoint`.
///
/// I/O-port BARs are intentionally excluded: passthrough resource matching
/// uses host MMIO ownership and never infers device identity from an IRQ.
///
/// # Errors
///
/// Returns an error when a required IRQ cannot be resolved or a memory BAR is
/// empty or wraps its `u64` exclusive end.
#[cfg(feature = "pci")]
pub fn binding_info_from_pci_endpoint(
    info: rdrive::probe::pci::PciInfo,
    endpoint: &rdrive::probe::pci::Endpoint,
    requirement: crate::PciIrqRequirement,
) -> Result<BindingInfo, OnProbeError> {
    let resources = binding_info_from_pci_endpoint_resources(info, endpoint)?;
    let ranges = resources.host_mmio_ranges().to_vec();
    binding_info_from_pci_resources(info, requirement, ranges)
}

#[cfg(feature = "pci")]
pub(crate) fn binding_info_from_pci_endpoint_resources(
    info: rdrive::probe::pci::PciInfo,
    endpoint: &rdrive::probe::pci::Endpoint,
) -> Result<BindingInfo, OnProbeError> {
    let ranges = host_mmio_ranges_from_pci_bars(info.address, endpoint.bars())?;
    Ok(BindingInfo::empty().with_host_resources(pci_locator(info), ranges))
}

fn resolve_fdt_irqs(info: &FdtInfo<'_>) -> Result<Vec<(usize, BindingIrq)>, OnProbeError> {
    info.interrupts()
        .into_iter()
        .enumerate()
        .map(|(source_id, interrupt)| {
            let controller = info
                .phandle_to_device_id(interrupt.interrupt_parent)
                .ok_or_else(|| {
                    OnProbeError::other(format!(
                        "interrupt source {source_id} parent {} is not registered",
                        interrupt.interrupt_parent
                    ))
                })?;
            Ok((
                source_id,
                binding_irq_from_fdt_interrupt(controller, interrupt.specifier),
            ))
        })
        .collect()
}

fn host_mmio_ranges_from_fdt(info: &FdtInfo<'_>) -> Result<Vec<HostMmioRange>, OnProbeError> {
    info.node
        .regs()
        .into_iter()
        .enumerate()
        .map(|(index, register)| {
            let length = register.size.ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] reg {index} does not declare a size",
                    info.node.path()
                ))
            })?;
            HostMmioRange::try_new(register.address, length).map_err(|error| {
                OnProbeError::other(format!(
                    "[{}] invalid reg {index}: {error}",
                    info.node.path()
                ))
            })
        })
        .collect()
}

fn host_mmio_ranges_from_acpi(
    path: &str,
    resources: &[rdrive::probe::acpi::AcpiResourceRange],
) -> Result<Vec<HostMmioRange>, OnProbeError> {
    resources
        .iter()
        .enumerate()
        .map(|(index, resource)| {
            HostMmioRange::try_new(resource.base, resource.size).map_err(|error| {
                OnProbeError::other(format!(
                    "[{path}] invalid ACPI memory range {index}: {error}"
                ))
            })
        })
        .collect()
}

fn binding_irq_from_fdt_interrupt(controller: DeviceId, cells: impl Into<Vec<u32>>) -> BindingIrq {
    BindingIrq::fdt_interrupt_with_controller(controller, cells)
}

#[cfg(feature = "pci")]
fn binding_info_from_pci_resources(
    info: rdrive::probe::pci::PciInfo,
    requirement: crate::PciIrqRequirement,
    ranges: Vec<HostMmioRange>,
) -> Result<BindingInfo, OnProbeError> {
    let irq = crate::pci::resolve_intx_binding(info)?;
    if irq.is_none() && requirement == crate::PciIrqRequirement::Required {
        return Err(OnProbeError::other(format!(
            "failed to resolve IRQ for PCI endpoint {}",
            info.address
        )));
    }
    Ok(BindingInfo::with_binding_irq(irq).with_host_resources(pci_locator(info), ranges))
}

#[cfg(feature = "pci")]
fn pci_locator(info: rdrive::probe::pci::PciInfo) -> BindingLocator {
    BindingLocator::Pci {
        segment: info.address.segment(),
        bus: info.address.bus(),
        device: info.address.device(),
        function: info.address.function(),
    }
}

#[cfg(feature = "pci")]
fn host_mmio_ranges_from_pci_bars(
    address: rdrive::probe::pci::PciAddress,
    bars: [Option<pcie::Bar>; 6],
) -> Result<Vec<HostMmioRange>, OnProbeError> {
    bars.into_iter()
        .enumerate()
        .filter_map(|(index, bar)| match bar {
            Some(pcie::Bar::Memory32 { address, size, .. }) => {
                Some((index, u64::from(address), u64::from(size)))
            }
            Some(pcie::Bar::Memory64 { address, size, .. }) => Some((index, address, size)),
            Some(pcie::Bar::Io { .. }) | None => None,
        })
        .map(|(index, base, length)| {
            HostMmioRange::try_new(base, length).map_err(|error| {
                OnProbeError::other(format!(
                    "PCI endpoint {address} has invalid BAR {index}: {error}"
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    #[cfg(feature = "pci")]
    use alloc::vec;

    #[cfg(feature = "pci")]
    use pcie::Bar;
    use rdrive::probe::acpi::AcpiResourceRange;
    #[cfg(feature = "pci")]
    use rdrive::probe::pci::PciAddress;

    use super::*;

    #[test]
    fn acpi_resources_validate_and_preserve_every_memory_range() {
        let ranges = host_mmio_ranges_from_acpi(
            "\\_SB.PCI0.NVM0",
            &[
                AcpiResourceRange {
                    base: 0x9000_0000,
                    size: 0x4000,
                },
                AcpiResourceRange {
                    base: 0x1_0000_0000,
                    size: 0x20_0000,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            ranges,
            [
                HostMmioRange::try_new(0x9000_0000, 0x4000).unwrap(),
                HostMmioRange::try_new(0x1_0000_0000, 0x20_0000).unwrap(),
            ]
        );
    }

    #[test]
    fn acpi_resources_reject_wrapping_range() {
        let error = host_mmio_ranges_from_acpi(
            "\\_SB.BAD0",
            &[AcpiResourceRange {
                base: u64::MAX - 0xff,
                size: 0x100,
            }],
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("\\_SB.BAD0"));
        assert!(message.contains("overflows"));
    }

    #[cfg(feature = "pci")]
    #[test]
    fn pci_resources_keep_all_memory_bars_and_ignore_io_bars() {
        let ranges = host_mmio_ranges_from_pci_bars(
            PciAddress::new(2, 3, 4, 5),
            [
                Some(Bar::Memory32 {
                    address: 0x8000_0000,
                    size: 0x1000,
                    prefetchable: false,
                }),
                Some(Bar::Io { port: 0xc000 }),
                Some(Bar::Memory64 {
                    address: 0x10_0000_0000,
                    size: 0x20_0000,
                    prefetchable: true,
                }),
                None,
                None,
                None,
            ],
        )
        .unwrap();

        assert_eq!(
            ranges,
            vec![
                HostMmioRange::try_new(0x8000_0000, 0x1000).unwrap(),
                HostMmioRange::try_new(0x10_0000_0000, 0x20_0000).unwrap(),
            ]
        );
    }

    #[cfg(feature = "pci")]
    #[test]
    fn pci_resources_reject_zero_sized_memory_bar() {
        let error = host_mmio_ranges_from_pci_bars(
            PciAddress::new(0, 0, 1, 0),
            [
                Some(Bar::Memory32 {
                    address: 0x8000_0000,
                    size: 0,
                    prefetchable: false,
                }),
                None,
                None,
                None,
                None,
                None,
            ],
        )
        .unwrap_err();

        assert!(error.to_string().contains("BAR 0"));
        assert!(error.to_string().contains("zero length"));
    }
}
