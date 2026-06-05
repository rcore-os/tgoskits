extern crate alloc;

#[cfg(all(
    target_os = "none",
    any(
        feature = "intel-net",
        feature = "ixgbe",
        feature = "realtek-rtl8125",
        feature = "virtio-net",
        feature = "xhci-pci",
    )
))]
use alloc::format;

use log::debug;
#[cfg(all(
    target_os = "none",
    any(
        feature = "intel-net",
        feature = "ixgbe",
        feature = "realtek-rtl8125",
        feature = "virtio-net",
        feature = "xhci-pci",
    )
))]
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

#[cfg(all(
    target_os = "none",
    any(
        feature = "intel-net",
        feature = "ixgbe",
        feature = "realtek-rtl8125",
        feature = "virtio-net",
        feature = "xhci-pci",
    )
))]
pub(crate) fn acpi_irq_for_endpoint(
    address: PciAddress,
    interrupt_pin: u8,
) -> Result<Option<usize>, OnProbeError> {
    let Some(result) =
        rdrive::probe::acpi::with_acpi(|acpi| acpi.pci_irq_for_endpoint(address, interrupt_pin))
    else {
        return Ok(None);
    };
    result
        .map(|route| {
            route.map(|route| {
                log::info!(
                    "ACPI PCI INTx route: endpoint {} pin {} -> GSI {} IOAPIC {} input {} vector \
                     {:#x}",
                    address,
                    interrupt_pin,
                    route.gsi,
                    route.io_apic_id,
                    route.io_apic_input,
                    route.vector
                );
                route.vector
            })
        })
        .map_err(|err| OnProbeError::other(format!("{err}")))
}
