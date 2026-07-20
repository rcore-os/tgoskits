extern crate alloc;

use alloc::format;

use log::info;
use pcie::CommandRegister;
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use super::{align_up_4k, register_usb_host_with_irq_lease, usb_kernel};
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint, pci::PciIntxIrqLease};

const DRIVER_NAME: &str = "usb-xhci-pci";

crate::model_register!(
    name: "USB xHCI PCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let class = probe.endpoint().revision_and_class();
    if (class.base_class, class.sub_class, class.interface) != (0x0c, 0x03, 0x30) {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = probe.endpoint().bar_mmio(0) else {
        return Err(OnProbeError::other("xHCI BAR0 MMIO region missing"));
    };
    let binding = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;

    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    let mmio = crate::mmio::iomap(bar.start, align_up_4k(bar.count().max(1)))?;
    let address = probe.endpoint().address();
    let host = crab_usb::USBHost::new_xhci(mmio, usb_kernel()).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create xHCI host for PCI endpoint {address}: {err}",
        ))
    })?;

    let endpoint = probe.take_endpoint();
    let irq_lease = PciIntxIrqLease::new(endpoint, binding);
    let irq = register_usb_host_with_irq_lease(
        probe.into_platform_device(),
        DRIVER_NAME,
        host,
        irq_lease,
    );
    info!(
        "xHCI PCI host registered successfully at {} with irq {:?}",
        address, irq
    );
    Ok(())
}
