extern crate alloc;

#[cfg(pci_dyn_acpi_intx_route)]
use alloc::format;

use log::debug;
#[cfg(pci_dyn_acpi_intx_route)]
use rdrive::probe::acpi::AcpiGsiRoute;
#[cfg(pci_dyn_acpi_intx_route)]
use rdrive::probe::pci::PciAddress;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiInfo},
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

fn probe_acpi_ecam(info: AcpiInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
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

#[cfg(pci_dyn_acpi_intx_route)]
pub(crate) fn acpi_irq_for_endpoint(
    address: PciAddress,
    interrupt_pin: u8,
) -> Result<Option<usize>, OnProbeError> {
    let Some(result) =
        rdrive::probe::acpi::with_acpi(|acpi| acpi.pci_irq_for_endpoint(address, interrupt_pin))
    else {
        return Ok(None);
    };
    let route = result.map_err(|err| OnProbeError::other(format!("{err}")))?;
    let Some(route) = route else {
        return Ok(None);
    };

    let irq = setup_acpi_intx_irq(&route.gsi)?;
    log::info!(
        "ACPI PCI INTx route: endpoint {} pin {} -> GSI {} IOAPIC {} input {} vector {:#x}",
        address,
        interrupt_pin,
        route.gsi.gsi,
        route.gsi.controller_id,
        route.gsi.controller_input,
        usize::from(irq)
    );
    Ok(Some(usize::from(irq)))
}

#[cfg(pci_dyn_acpi_intx_route)]
fn setup_acpi_intx_irq(route: &AcpiGsiRoute) -> Result<rdrive::IrqId, OnProbeError> {
    let intc = rdrive::get_list::<rdif_intc::Intc>()
        .into_iter()
        .find(|intc| intc.descriptor().name.starts_with("ACPI IOAPIC"))
        .ok_or_else(|| OnProbeError::other("ACPI IOAPIC interrupt controller is not registered"))?;
    let mut intc = intc
        .lock()
        .map_err(|_| OnProbeError::other("ACPI IOAPIC interrupt controller is locked"))?;
    Ok(intc.setup_irq_by_acpi(route))
}
