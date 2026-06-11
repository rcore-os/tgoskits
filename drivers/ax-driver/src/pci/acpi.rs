extern crate alloc;

#[cfg(plat_dyn)]
use alloc::format;

use log::debug;
#[cfg(plat_dyn)]
use rdrive::probe::pci::PciInfo;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
    },
};

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
    let (info, plat_dev) = probe.into_parts();
    let mut registered = false;
    for region in info.root.pci_ecam_regions() {
        debug!("ACPI MCFG PCI ECAM region: {region:?}");
        super::register_ecam_controller(
            PlatformDevice {
                descriptor: plat_dev.descriptor.clone(),
            },
            region.base_address as usize,
            region.size(),
            None,
            None,
        )?;
        registered = true;
    }

    if registered {
        Ok(())
    } else {
        Err(OnProbeError::NotMatch)
    }
}

#[cfg(plat_dyn)]
pub(crate) fn acpi_irq_for_endpoint(info: PciInfo) -> Result<Option<usize>, OnProbeError> {
    let Some(result) = rdrive::probe::acpi::with_acpi(|acpi| acpi.pci_irq_for_endpoint(info))
    else {
        return Ok(None);
    };
    let route = result.map_err(|err| OnProbeError::other(format!("{err}")))?;
    let Some(route) = route else {
        return Ok(None);
    };

    let irq = crate::binding_info_from_acpi_route("PCI endpoint", Some(route.gsi))?
        .irq_num()
        .expect("Some ACPI GSI route must resolve to an IRQ");
    log::info!(
        "ACPI PCI INTx route: endpoint {} pin {} -> GSI {} {:?} {} input {} vector {:#x}",
        info.address,
        route.intx_route.root_pin,
        route.gsi.gsi,
        route.gsi.controller,
        route.gsi.controller_id,
        route.gsi.controller_input,
        irq
    );
    Ok(Some(irq))
}
