extern crate alloc;

use alloc::format;

use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

use super::{PlatformDeviceUsbHost, USB_KERNEL, decode_fdt_irq};
use crate::drivers::iomap;

const DRIVER_NAME: &str = "usb-xhci-mmio";

module_driver!(
    name: "USB xHCI MMIO",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["generic-xhci", "xhci-platform"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio = iomap((base_reg.address as usize).into(), mmio_size)?;
    let interrupts = info.interrupts();
    let irq_num = decode_fdt_irq(&interrupts);

    let host = crab_usb::USBHost::new_xhci(mmio, &USB_KERNEL).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create xHCI host for [{}]: {err}",
            info.node.name()
        ))
    })?;

    plat_dev.register_usb_host(DRIVER_NAME, host, irq_num);
    info!(
        "xHCI MMIO host registered successfully for {} with irq {:?}",
        info.node.name(),
        irq_num
    );
    Ok(())
}
