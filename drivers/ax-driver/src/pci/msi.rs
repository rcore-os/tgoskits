extern crate alloc;

use alloc::{format, vec::Vec};

use fdt_edit::{Fdt, NodeType, Phandle};
use log::warn;
use pcie::{Endpoint, MsixError, MsixTableRegion};
use rdif_msi::{Msi, MsiAllocation, MsiDeviceId, MsiRequest};
use rdrive::{
    DeviceId,
    probe::{
        OnProbeError,
        pci::{PciAddress, PciInfo},
    },
};

use crate::{BindingInfo, BindingIrq, BindingIrqBinding};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciMsiTarget {
    pub provider: DeviceId,
    pub device: MsiDeviceId,
}

pub struct PciIrqLease {
    provider: DeviceId,
    allocation: Option<MsiAllocation>,
    table: MsixTableRegion,
    _table_mmio: mmio_api::Mmio,
}

pub type PciMsixAllocation = PciIrqLease;

impl PciIrqLease {
    pub fn allocate(
        endpoint: &mut Endpoint,
        info: PciInfo,
        vector_count: u16,
    ) -> Result<Self, OnProbeError> {
        let target = msi_target_for_endpoint(info)?;
        let table_info = endpoint.msix_table_info().map_err(msix_probe_error)?;
        let table_range = endpoint.msix_table_range().map_err(msix_probe_error)?;

        if vector_count == 0 || vector_count > table_info.entries {
            return Err(OnProbeError::other(format!(
                "PCI endpoint {} requested {vector_count} MSI-X vectors, table has {}",
                info.address, table_info.entries
            )));
        }

        let provider = rdrive::get::<Msi>(target.provider)
            .map_err(|err| msi_provider_lookup_error(info.address, target.provider, err))?;
        let mut provider = provider
            .lock()
            .map_err(|_| OnProbeError::other("failed to lock MSI provider"))?;
        let mut allocation = Some(
            provider
                .allocate(MsiRequest::new(target.device, vector_count))
                .map_err(|err| {
                    OnProbeError::other(format!(
                        "failed to allocate {vector_count} MSI-X vectors for {}: {err:?}",
                        info.address
                    ))
                })?,
        );

        let setup = (|| {
            let table_mmio = axklib::mmio::ioremap(table_range.start.into(), table_range.len())
                .map_err(|err| OnProbeError::other(format!("failed to map MSI-X table: {err}")))?;
            let table =
                unsafe { MsixTableRegion::new(table_mmio.as_nonnull_ptr(), table_info.entries) };

            endpoint
                .set_msix_function_mask(true)
                .map_err(msix_probe_error)?;
            {
                let allocation_ref = allocation
                    .as_ref()
                    .ok_or_else(|| OnProbeError::other("MSI-X allocation was already consumed"))?;
                for vector in allocation_ref.vectors() {
                    let message = provider.compose_message(vector).map_err(|err| {
                        OnProbeError::other(format!(
                            "failed to compose MSI-X message for {} vector {:?}: {err:?}",
                            info.address, vector.index
                        ))
                    })?;
                    table
                        .program_masked(vector.index.0, message)
                        .map_err(msix_probe_error)?;
                    provider.set_vector_enabled(vector, false).map_err(|err| {
                        OnProbeError::other(format!("failed to disable MSI vector: {err:?}"))
                    })?;
                }
            }
            endpoint.set_msix_enabled(true).map_err(msix_probe_error)?;

            Ok(Self {
                provider: target.provider,
                allocation: allocation.take(),
                table,
                _table_mmio: table_mmio,
            })
        })();

        if setup.is_err()
            && let Some(allocation) = allocation.take()
            && let Err(err) = provider.free(allocation)
        {
            warn!(
                "failed to roll back MSI-X allocation for {} after setup error: {err:?}",
                info.address
            );
        }
        setup
    }

    pub fn binding_info(&self) -> BindingInfo {
        binding_info_from_msi_vectors(self.vectors())
    }

    pub fn irq_bindings(&self) -> Vec<BindingIrqBinding> {
        self.binding_info().irq_sources().to_vec()
    }

    pub fn vector_indices(&self) -> Vec<u16> {
        self.vectors().iter().map(|vector| vector.index.0).collect()
    }

    pub fn enable(&self) {
        if let Some(allocation) = &self.allocation
            && let Ok(provider) = rdrive::get::<Msi>(self.provider)
            && let Ok(mut provider) = provider.lock()
        {
            for vector in allocation.vectors() {
                if let Err(err) = provider.set_vector_enabled(vector, true) {
                    warn!("failed to enable MSI vector {:?}: {err:?}", vector.index);
                }
                if let Err(err) = self.table.unmask(vector.index.0) {
                    warn!(
                        "failed to unmask MSI-X table entry {:?}: {err}",
                        vector.index
                    );
                }
            }
        }
    }

    pub fn disable(&self) {
        if let Some(allocation) = &self.allocation {
            for vector in allocation.vectors() {
                if let Err(err) = self.table.mask(vector.index.0) {
                    warn!("failed to mask MSI-X table entry {:?}: {err}", vector.index);
                }
            }
            if let Ok(provider) = rdrive::get::<Msi>(self.provider)
                && let Ok(mut provider) = provider.lock()
            {
                for vector in allocation.vectors() {
                    if let Err(err) = provider.set_vector_enabled(vector, false) {
                        warn!("failed to disable MSI vector {:?}: {err:?}", vector.index);
                    }
                }
            }
        }
    }

    fn vectors(&self) -> &[rdif_msi::MsiVector] {
        self.allocation
            .as_ref()
            .map(MsiAllocation::vectors)
            .unwrap_or(&[])
    }
}

impl crate::IrqBindingLease for PciIrqLease {
    fn binding_info(&self) -> BindingInfo {
        PciIrqLease::binding_info(self)
    }

    fn enable_binding_irq(&self) {
        self.enable();
    }

    fn disable_binding_irq(&self) {
        self.disable();
    }
}

impl Drop for PciIrqLease {
    fn drop(&mut self) {
        self.disable();
        let Some(allocation) = self.allocation.take() else {
            return;
        };
        if let Ok(provider) = rdrive::get::<Msi>(self.provider)
            && let Ok(mut provider) = provider.lock()
            && let Err(err) = provider.free(allocation)
        {
            warn!("failed to free MSI-X allocation: {err:?}");
        }
    }
}

fn binding_info_from_msi_vectors(vectors: &[rdif_msi::MsiVector]) -> BindingInfo {
    let irqs = vectors
        .iter()
        .map(|vector| (usize::from(vector.index.0), BindingIrq::id(vector.irq)));
    BindingInfo::with_irq_sources(irqs)
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
enum DynamicMsiSource {
    Acpi,
    Fdt,
}

fn dynamic_msi_source() -> Option<DynamicMsiSource> {
    if rdrive::probe::acpi::with_acpi(|_| ()).is_some() {
        Some(DynamicMsiSource::Acpi)
    } else if rdrive::with_fdt(|_| ()).is_some() {
        Some(DynamicMsiSource::Fdt)
    } else {
        None
    }
}

fn fdt_msi_target_for_endpoint(info: PciInfo) -> Result<PciMsiTarget, OnProbeError> {
    let Some(result) = rdrive::with_fdt(|fdt| resolve_fdt_msi_target(fdt, info)) else {
        return Err(OnProbeError::Unsupported("live FDT not found"));
    };
    result
}

fn resolve_fdt_msi_target(fdt: &Fdt, info: PciInfo) -> Result<PciMsiTarget, OnProbeError> {
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

fn fdt_pci_host_for_endpoint(
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

fn resolve_msi_parent(
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

fn resolve_msi_map(host: &fdt_edit::Node, rid: u32) -> Result<Option<PciMsiTarget>, OnProbeError> {
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

fn msi_cells_for_phandle(phandle: Phandle) -> Option<usize> {
    rdrive::with_fdt(|fdt| {
        fdt.get_by_phandle(phandle)
            .and_then(|node| node.as_node().get_property("#msi-cells"))
            .and_then(|prop| prop.get_u32())
            .map(|cells| cells as usize)
    })
    .flatten()
}

fn provider_for_phandle(phandle: Phandle) -> Result<DeviceId, OnProbeError> {
    rdrive::fdt_phandle_to_device_id(phandle).ok_or(OnProbeError::Unsupported(
        "PCI MSI provider phandle is not registered",
    ))
}

fn msi_provider_lookup_error(
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

fn pci_requester_id(address: PciAddress) -> u32 {
    (u32::from(address.bus()) << 8)
        | (u32::from(address.device()) << 3)
        | u32::from(address.function())
}

fn msix_probe_error(err: MsixError) -> OnProbeError {
    OnProbeError::other(format!("{err}"))
}

#[cfg(test)]
mod tests {
    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_msi::{MsiEventId, MsiVector, MsiVectorIndex};

    use super::*;

    #[test]
    fn requester_id_uses_bus_device_function() {
        let address = PciAddress::new(0, 3, 4, 2);
        assert_eq!(pci_requester_id(address), 0x322);
    }

    #[test]
    fn msi_map_matches_masked_requester_id() {
        let mut host = fdt_edit::Node::new("pcie@0");
        host.set_property(prop_u32s("msi-map-mask", &[0xff]));
        host.set_property(prop_u32s("msi-map", &[0x40, 1, 0x1000, 0x40]));

        // Provider phandle lookup is intentionally outside this pure parser
        // test; the tuple walk should reject non-matching masked RIDs first.
        assert!(resolve_msi_map(&host, 0x20).unwrap().is_none());
    }

    #[test]
    fn missing_msi_provider_is_unsupported_for_legacy_fallback() {
        let err = msi_provider_lookup_error(
            PciAddress::new(0, 0, 1, 0),
            DeviceId::from(7),
            rdrive::GetDeviceError::NotFound,
        );

        assert!(matches!(
            err,
            OnProbeError::Unsupported("PCI MSI provider is not registered")
        ));
    }

    #[test]
    fn non_msi_controller_interface_is_unsupported_for_legacy_fallback() {
        let err = msi_provider_lookup_error(
            PciAddress::new(0, 0, 1, 0),
            DeviceId::from(7),
            rdrive::GetDeviceError::TypeNotMatch,
        );

        assert!(matches!(
            err,
            OnProbeError::Unsupported("PCI MSI provider interface is unavailable")
        ));
    }

    #[test]
    fn binding_info_uses_leaf_irq_not_parent_lpi() {
        let parent_irq = IrqId::new(IrqDomainId(7), HwIrq(8192));
        let leaf_irq = IrqId::new(IrqDomainId(8), HwIrq(0));
        let info = binding_info_from_msi_vectors(&[MsiVector::with_parent(
            MsiVectorIndex(0),
            MsiEventId(32),
            leaf_irq,
            parent_irq,
        )]);

        assert_eq!(
            info.irq_sources(),
            &[BindingIrqBinding {
                source_id: 0,
                irq: BindingIrq::id(leaf_irq),
            }]
        );
    }

    fn prop_u32s(name: &str, values: &[u32]) -> fdt_edit::Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        fdt_edit::Property::new(name, data)
    }
}
