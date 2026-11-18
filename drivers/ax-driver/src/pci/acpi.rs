extern crate alloc;

use alloc::format;

use log::debug;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
        pci::PciInfo,
    },
};

use crate::BindingIrq;

crate::model_register!(
    name: "ACPI Generic PCIe Controller Driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Acpi {
            ids: &[AcpiId {
                hid: "PNP0A08",
                cids: &["PNP0A03"],
            }],
            on_probe: probe_acpi_ecam
        }
    ],
);

fn probe_acpi_ecam(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    if !super::has_pci_endpoint_drivers() {
        return Err(OnProbeError::NotMatch);
    }
    let (info, plat_dev) = probe.into_parts();
    let mut registered = false;
    for region in info.root.pci_ecam_regions() {
        debug!("ACPI MCFG PCI ECAM region: {region:?}");
        let dma_coherent = matches!(
            crate::binding_resolver::dma_coherency_from_acpi_cca(region.dma_coherent)?,
            dma_api::DmaCoherency::Coherent
        );
        let base = usize::try_from(region.base_address).map_err(|_| {
            OnProbeError::other("ACPI PCI configuration address exceeds pointer width")
        })?;
        let driver = pcie::PcieGeneric::new_with_layout(
            base,
            region.size(),
            region.layout,
            axklib::mmio::op(),
        )
        .map_err(|error| {
            OnProbeError::other(format!(
                "failed to map ACPI PCI configuration window: {error}"
            ))
        })?;
        super::register_config_controller(
            PlatformDevice {
                descriptor: plat_dev.descriptor.clone(),
            },
            pcie::PcieController::new(driver),
            None,
            None,
            dma_coherent,
        );
        log::info!(
            "registered ACPI PCI configuration window: segment={} buses={}..={} base={:#x} \
             layout={:?}",
            region.segment_group,
            region.bus_start,
            region.bus_end,
            region.base_address,
            region.layout,
        );
        registered = true;
    }

    if registered {
        Ok(())
    } else {
        Err(OnProbeError::NotMatch)
    }
}

pub(crate) fn acpi_irq_for_endpoint(info: PciInfo) -> Result<Option<BindingIrq>, OnProbeError> {
    let Some(result) = rdrive::probe::acpi::with_acpi(|acpi| acpi.pci_irq_for_endpoint(info))
    else {
        return Ok(None);
    };
    let route = result.map_err(|err| OnProbeError::other(format!("{err}")))?;
    let Some(route) = route else {
        return Ok(None);
    };

    log::info!(
        "ACPI PCI INTx route: endpoint {} pin {} -> GSI {} {:?} {} input {}",
        info.address,
        route.intx_route.root_pin,
        route.gsi.gsi,
        route.gsi.controller,
        route.gsi.controller_id,
        route.gsi.controller_input,
    );
    Ok(Some(BindingIrq::from(route.gsi)))
}
