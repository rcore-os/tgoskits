use alloc::format;

use log::info;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use some_serial::pl011;

use super::{PlatformSerialDevice, erase_uart, prop_u32, serial_device_info};

model_register!(
    name: "PL011 serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl011"],
        on_probe: probe
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();

    info!("Probing PL011 serial device: {}", info.node.name());
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = crate::mmio::iomap(base_reg.address as usize, mmio_size as usize)?;
    let clock_freq = prop_u32(info.node.as_node(), "clock-frequency").unwrap_or(24_000_000);
    let raw = pl011::Pl011::new(mmio_base, clock_freq);
    let serial = erase_uart(raw);
    let base = serial.hardware.register_base;
    let device_info = serial_device_info(&info, &base_reg);

    info!("PL011 serial@{base:#x} registered successfully");
    plat_dev.register(PlatformSerialDevice::new(
        serial,
        device_info.path,
        device_info.alias_index,
        device_info.paddr,
        device_info.irq,
    ));
    Ok(())
}
