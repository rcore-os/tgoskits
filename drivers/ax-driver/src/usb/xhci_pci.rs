extern crate alloc;

use alloc::format;

use log::info;
use pcie::CommandRegister;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
    },
};

use super::{PlatformDeviceUsbHost, align_up_4k, pci_irq_or_error, usb_kernel};

const DRIVER_NAME: &str = "usb-xhci-pci";

module_driver!(
    name: "USB xHCI PCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe as FnOnProbe
    }],
);

fn probe(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let class = endpoint.revision_and_class();
    if (class.base_class, class.sub_class, class.interface) != (0x0c, 0x03, 0x30) {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("xHCI BAR0 MMIO region missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });

    let mmio = crate::mmio::iomap(bar.start, align_up_4k(bar.count().max(1)))?;
    let irq_num = Some(pci_irq_or_error(endpoint)?);
    let host = crab_usb::USBHost::new_xhci(mmio, usb_kernel()).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create xHCI host for PCI endpoint {}: {err}",
            endpoint.address()
        ))
    })?;

    plat_dev.register_usb_host(DRIVER_NAME, host, irq_num);
    info!(
        "xHCI PCI host registered successfully at {} with irq {:?}",
        endpoint.address(),
        irq_num
    );
    Ok(())
}
