extern crate alloc;

use alloc::format;

use log::info;
use rdrive::{probe::OnProbeError, register::ProbeFdt};

use super::{ProbeFdtUsbHost, usb_kernel};

const DRIVER_NAME: &str = "usb-xhci-mmio";

crate::model_register!(
    name: "USB xHCI MMIO",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["generic-xhci", "xhci-platform"],
        on_probe: probe
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let node_name = probe.info().node.name();
    let base_reg = probe
        .info()
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(alloc::format!("[{}] has no reg", node_name)))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio = crate::mmio::iomap(base_reg.address as usize, mmio_size)?;

    let host = crab_usb::USBHost::new_xhci(mmio, usb_kernel()).map_err(|err| {
        OnProbeError::other(format!(
            "failed to create xHCI host for [{}]: {err}",
            node_name
        ))
    })?;

    let irq = probe.register_usb_host(DRIVER_NAME, host)?;
    info!(
        "xHCI MMIO host registered successfully for {} with irq {:?}",
        node_name, irq
    );
    Ok(())
}
