extern crate alloc;

use alloc::format;

use log::info;
use pcie::CommandRegister;
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use super::{PlatformDeviceUsbHost, align_up_4k, usb_kernel};
use crate::BindingInfo;

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
    let endpoint = probe.endpoint_mut();
    let class = endpoint.revision_and_class();
    if (class.base_class, class.sub_class, class.interface) != (0x0c, 0x03, 0x30) {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("xHCI BAR0 MMIO region missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    let mmio = crate::mmio::iomap(bar.start, align_up_4k(bar.count().max(1)))?;
    let address = endpoint.address();
    let host = crab_usb::USBHost::new_xhci(mmio, usb_kernel()).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create xHCI host for PCI endpoint {address}: {err}",
        ))
    })?;

    let irq = probe.into_platform_device().register_usb_host_with_info(
        DRIVER_NAME,
        host,
        BindingInfo::empty(),
    );
    info!(
        "xHCI PCI host registered successfully at {} with irq {:?}",
        address, irq
    );
    Ok(())
}
