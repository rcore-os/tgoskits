use alloc::format;

use log::info;
use rdrive::{
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiInfo, ProbeAcpi},
    },
    register::ProbeFdt,
};
use some_serial::ns16550 as serial_ns16550;

use super::{
    PlatformSerialDevice, SerialProbeRuntime, acpi_serial_device_info, prop_u32,
    serial_device_info, serial_runtime,
};

const ACPI_NS16550_CLOCK: u32 = 1_843_200;
const ACPI_NS16550_REG_WIDTH: usize = 1;

model_register!(
    name: "NS16550 serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["snps,dw-apb-uart", "ns16550a", "ns16550"],
            on_probe: probe
        },
        ProbeKind::Acpi {
            ids: &[
                AcpiId {
                    hid: "PNP0501",
                    cids: &[],
                },
                AcpiId {
                    hid: "PNP0500",
                    cids: &[],
                },
            ],
            on_probe: probe_acpi
        },
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();

    info!("Probing NS16550 serial device: {}", info.node.name());
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = crate::mmio::iomap_firmware_device(
        "NS16550 serial",
        base_reg.address as usize,
        mmio_size as usize,
    )?;
    let node = info.node.as_node();
    let reg_width = prop_u32(node, "reg-io-width").unwrap_or(1) as usize;
    let reg_shift = prop_u32(node, "reg-shift").map(|shift| 1usize << shift);
    let ns16550_width = reg_shift.unwrap_or(reg_width);
    let mut serial: Option<SerialProbeRuntime> = None;

    for compatible in node.compatibles() {
        if compatible == "snps,dw-apb-uart" {
            let clock_freq = prop_u32(node, "clock-frequency")
                .unwrap_or(serial_ns16550::dw_apb::SG2002_UART_CLOCK);
            let raw = serial_ns16550::DwApbUart::new_raw(mmio_base, clock_freq);
            serial = Some(serial_runtime(raw));
            break;
        }

        if matches!(compatible, "ns16550a" | "ns16550") {
            let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(24_000_000);
            let raw = serial_ns16550::Ns16550::new_mmio(mmio_base, clock_freq, ns16550_width);
            serial = Some(serial_runtime(raw));
            break;
        }
    }

    let serial = serial.ok_or(OnProbeError::NotMatch)?;
    let device_info = serial_device_info(&info, &base_reg, serial.base_addr, serial.baudrate);

    info!(
        "NS16550 serial@{:#x} registered successfully",
        serial.base_addr
    );
    plat_dev.register(PlatformSerialDevice::new(
        serial.name.into(),
        device_info,
        serial.runtime,
    ));
    Ok(())
}

struct AcpiSerialResource {
    serial: SerialProbeRuntime,
    paddr: usize,
    mapped_base: usize,
}

fn probe_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();

    info!("Probing ACPI NS16550 serial device: {}", info.path);
    let resource = if let Some(resource) = acpi_io_serial(info)? {
        resource
    } else {
        acpi_mmio_serial(info)?
    };
    let device_info = acpi_serial_device_info(
        info,
        resource.paddr,
        resource.mapped_base,
        resource.serial.baudrate,
    );
    let serial_name = resource.serial.name.into();
    let plat_dev = probe.into_platform_device();

    info!(
        "ACPI NS16550 serial@{:#x} registered successfully",
        resource.paddr
    );
    plat_dev.register(PlatformSerialDevice::new(
        serial_name,
        device_info,
        resource.serial.runtime,
    ));
    Ok(())
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn acpi_io_serial(info: &AcpiInfo<'_>) -> Result<Option<AcpiSerialResource>, OnProbeError> {
    let Some(range) = info.io_ranges().first() else {
        return Ok(None);
    };
    let port = u16::try_from(range.base).map_err(|_| {
        OnProbeError::other(format!(
            "{} has invalid ACPI serial I/O base {:#x}",
            info.path, range.base
        ))
    })?;
    let raw = serial_ns16550::Ns16550::new_port(port, ACPI_NS16550_CLOCK);
    let serial = serial_runtime(raw);
    let mapped_base = serial.base_addr;
    Ok(Some(AcpiSerialResource {
        serial,
        paddr: usize::from(port),
        mapped_base,
    }))
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn acpi_io_serial(_info: &AcpiInfo<'_>) -> Result<Option<AcpiSerialResource>, OnProbeError> {
    Ok(None)
}

fn acpi_mmio_serial(info: &AcpiInfo<'_>) -> Result<AcpiSerialResource, OnProbeError> {
    let range = info.memory_ranges().first().ok_or_else(|| {
        OnProbeError::other(format!(
            "{} has no ACPI serial I/O port or MMIO resource",
            info.path
        ))
    })?;
    let paddr = usize::try_from(range.base).map_err(|_| {
        OnProbeError::other(format!(
            "{} has invalid ACPI serial MMIO base {:#x}",
            info.path, range.base
        ))
    })?;
    let mmio_size = usize::try_from(range.size).unwrap_or(0x1000).max(0x1000);
    let mmio_base = crate::mmio::iomap(paddr, mmio_size)?;
    let raw =
        serial_ns16550::Ns16550::new_mmio(mmio_base, ACPI_NS16550_CLOCK, ACPI_NS16550_REG_WIDTH);
    let serial = serial_runtime(raw);
    let mapped_base = serial.base_addr;
    Ok(AcpiSerialResource {
        serial,
        paddr,
        mapped_base,
    })
}
