//! Dynamic-platform PCI requester to MSI provider routing.

use alloc::{format, vec::Vec};

use fdt_edit::{Fdt, NodeType, Phandle};
#[cfg(feature = "nvme")]
use pcie::MsixError;
use rdif_msi::MsiDeviceId;
use rdrive::{
    DeviceId,
    probe::{
        OnProbeError,
        pci::{PciAddress, PciInfo},
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciMsiTarget {
    pub provider: DeviceId,
    pub device: MsiDeviceId,
}

pub fn msi_target_for_endpoint(info: PciInfo) -> Result<PciMsiTarget, OnProbeError> {
    match dynamic_msi_source() {
        Some(DynamicMsiSource::Fdt) => fdt_msi_target_for_endpoint(info),
        Some(DynamicMsiSource::Acpi) => Err(OnProbeError::Unsupported(
            "ACPI IORT PCI MSI routing is not implemented",
        )),
        None => Err(OnProbeError::Unsupported(
            "PCI MSI routing requires FDT msi-parent/msi-map or ACPI IORT",
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DynamicMsiSource {
    Acpi,
    Fdt,
}

pub(super) fn dynamic_msi_source() -> Option<DynamicMsiSource> {
    if rdrive::probe::acpi::with_acpi(|_| ()).is_some() {
        Some(DynamicMsiSource::Acpi)
    } else if rdrive::with_fdt(|_| ()).is_some() {
        Some(DynamicMsiSource::Fdt)
    } else {
        None
    }
}

pub(super) fn fdt_msi_target_for_endpoint(info: PciInfo) -> Result<PciMsiTarget, OnProbeError> {
    let Some(result) = rdrive::with_fdt(|fdt| resolve_fdt_msi_target(fdt, info)) else {
        return Err(OnProbeError::Unsupported("live FDT not found"));
    };
    result
}

pub(super) fn resolve_fdt_msi_target(
    fdt: &Fdt,
    info: PciInfo,
) -> Result<PciMsiTarget, OnProbeError> {
    let host = fdt_pci_host_for_endpoint(fdt, info)?;
    let rid = pci_requester_id(info.address);

    let host_node = fdt
        .node(host.id())
        .ok_or_else(|| OnProbeError::other("PCI host node disappeared while resolving MSI"))?;

    if let Some(target) = resolve_msi_map(host_node, rid)? {
        return Ok(target);
    }
    if let Some(target) = resolve_msi_parent(host_node, rid)? {
        return Ok(target);
    }

    Err(OnProbeError::Unsupported("PCI host has no FDT MSI routing"))
}

pub(super) fn fdt_pci_host_for_endpoint(
    fdt: &Fdt,
    info: PciInfo,
) -> Result<fdt_edit::PciNodeView<'_>, OnProbeError> {
    let bus = info.address.bus();
    let mut candidates = Vec::new();
    let mut exact_range_matches = Vec::new();

    for node in fdt.all_nodes() {
        let NodeType::Pci(pci) = node else {
            continue;
        };
        match pci.bus_range() {
            Some(range) if range.contains(&(bus as u32)) => {
                exact_range_matches.push(pci);
                candidates.push(pci);
            }
            Some(_) => {}
            None => candidates.push(pci),
        }
    }

    if exact_range_matches.len() == 1 {
        Ok(exact_range_matches[0])
    } else if exact_range_matches.len() > 1 {
        Err(OnProbeError::other(format!(
            "multiple PCI host nodes in FDT match endpoint {} with the same bus-range",
            info.address
        )))
    } else if candidates.len() == 1 {
        Ok(candidates[0])
    } else if candidates.is_empty() {
        Err(OnProbeError::other(format!(
            "no PCI host node in FDT matches endpoint {}",
            info.address
        )))
    } else {
        Err(OnProbeError::other(format!(
            "multiple PCI host nodes in FDT match endpoint {} without a unique bus-range match",
            info.address
        )))
    }
}

pub(super) fn resolve_msi_parent(
    host: &fdt_edit::Node,
    rid: u32,
) -> Result<Option<PciMsiTarget>, OnProbeError> {
    let Some(prop) = host.get_property("msi-parent") else {
        return Ok(None);
    };
    let phandle = prop
        .get_u32_iter()
        .next()
        .map(Phandle::from)
        .ok_or_else(|| OnProbeError::other("PCI host msi-parent is empty"))?;
    Ok(Some(PciMsiTarget {
        provider: provider_for_phandle(phandle)?,
        device: MsiDeviceId(rid),
    }))
}

pub(super) fn resolve_msi_map(
    host: &fdt_edit::Node,
    rid: u32,
) -> Result<Option<PciMsiTarget>, OnProbeError> {
    let Some(prop) = host.get_property("msi-map") else {
        return Ok(None);
    };
    let mask = host
        .get_property("msi-map-mask")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(u32::MAX);
    let masked_rid = rid & mask;
    let cells: Vec<u32> = prop.get_u32_iter().collect();
    let mut offset = 0;
    while offset + 3 <= cells.len() {
        let rid_base = cells[offset] & mask;
        let phandle = Phandle::from(cells[offset + 1]);
        let msi_cells = msi_cells_for_phandle(phandle).unwrap_or(1);
        let tuple_len = 3 + msi_cells;
        if offset + tuple_len > cells.len() {
            return Err(OnProbeError::other("truncated PCI msi-map entry"));
        }
        let msi_base = cells[offset + 2];
        let length = cells[offset + 2 + msi_cells];
        let rid_end = rid_base
            .checked_add(length)
            .ok_or_else(|| OnProbeError::other("PCI msi-map rid range overflows u32"))?;
        if masked_rid >= rid_base && masked_rid < rid_end {
            let device = msi_base
                .checked_add(masked_rid - rid_base)
                .ok_or_else(|| OnProbeError::other("PCI msi-map device id overflows u32"))?;
            return Ok(Some(PciMsiTarget {
                provider: provider_for_phandle(phandle)?,
                device: MsiDeviceId(device),
            }));
        }
        offset += tuple_len;
    }
    Ok(None)
}

pub(super) fn msi_cells_for_phandle(phandle: Phandle) -> Option<usize> {
    rdrive::with_fdt(|fdt| {
        fdt.get_by_phandle(phandle)
            .and_then(|node| node.as_node().get_property("#msi-cells"))
            .and_then(|prop| prop.get_u32())
            .map(|cells| cells as usize)
    })
    .flatten()
}

pub(super) fn provider_for_phandle(phandle: Phandle) -> Result<DeviceId, OnProbeError> {
    rdrive::fdt_phandle_to_device_id(phandle).ok_or(OnProbeError::Unsupported(
        "PCI MSI provider phandle is not registered",
    ))
}

#[cfg(feature = "nvme")]
pub(super) fn msi_provider_lookup_error(
    address: PciAddress,
    provider: DeviceId,
    err: rdrive::GetDeviceError,
) -> OnProbeError {
    match err {
        rdrive::GetDeviceError::NotFound => {
            OnProbeError::Unsupported("PCI MSI provider is not registered")
        }
        rdrive::GetDeviceError::TypeNotMatch | rdrive::GetDeviceError::DeviceReleased => {
            OnProbeError::Unsupported("PCI MSI provider interface is unavailable")
        }
        rdrive::GetDeviceError::UsedByOthers(_) | rdrive::GetDeviceError::UsedByUnknown => {
            OnProbeError::other(format!(
                "PCI endpoint {address} MSI provider {provider:?} is busy: {err}"
            ))
        }
    }
}

pub(super) fn pci_requester_id(address: PciAddress) -> u32 {
    (u32::from(address.bus()) << 8)
        | (u32::from(address.device()) << 3)
        | u32::from(address.function())
}

#[cfg(feature = "nvme")]
pub(super) fn msix_probe_error(err: MsixError) -> OnProbeError {
    OnProbeError::other(format!("{err}"))
}
