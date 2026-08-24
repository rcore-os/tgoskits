use alloc::format;

use log::info;
use rdrive::{
    probe::{
        OnProbeError,
        acpi::{
            AcpiGsiRoute, AcpiId, AcpiInfo, AcpiSerialAddressSpace, AcpiSerialConsole,
            AcpiSerialInterface, ProbeAcpi,
        },
    },
    register::ProbeFdt,
};
use some_serial::ns16550 as serial_ns16550;

use super::{
    PlatformSerialDevice, ProbedUart, acpi_serial_device_info, erase_uart, prop_u32,
    serial_device_info,
};

const ACPI_NS16550_CLOCK: u32 = 1_843_200;
const ACPI_NS16550_REG_WIDTH: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpcrNs16550Spec {
    pub address_space: AcpiSerialAddressSpace,
    pub register_base: usize,
    pub register_size: usize,
    pub register_width: usize,
    pub clock_hz: u32,
    pub initial_baudrate: u32,
    pub irq_route: AcpiGsiRoute,
}

pub(super) fn spcr_ns16550_spec(console: &AcpiSerialConsole) -> Option<SpcrNs16550Spec> {
    if console.interface != AcpiSerialInterface::Uart16550 {
        return None;
    }
    let register_width = match console.access_size {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        _ => ACPI_NS16550_REG_WIDTH,
    };
    Some(SpcrNs16550Spec {
        address_space: console.address_space,
        register_base: usize::try_from(console.registers.base).ok()?,
        register_size: usize::try_from(console.registers.size).ok()?,
        register_width,
        clock_hz: console.clock_hz.unwrap_or(ACPI_NS16550_CLOCK),
        initial_baudrate: console.baud_rate.unwrap_or(115_200),
        irq_route: console.irq_route?,
    })
}

pub(super) fn uart_from_spcr(
    console: &AcpiSerialConsole,
) -> Result<Option<(ProbedUart, SpcrNs16550Spec)>, OnProbeError> {
    let Some(spec) = spcr_ns16550_spec(console) else {
        return Ok(None);
    };
    let raw = match spec.address_space {
        AcpiSerialAddressSpace::Memory => {
            let size = spec
                .register_size
                .max(spec.register_width.saturating_mul(8));
            let mmio_base = crate::mmio::iomap(spec.register_base, size)?;
            erase_uart(serial_ns16550::Ns16550::new_mmio(
                mmio_base,
                spec.clock_hz,
                spec.register_width,
            ))
        }
        AcpiSerialAddressSpace::Io => return uart_from_spcr_io(spec),
    };
    Ok(Some((raw, spec)))
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn uart_from_spcr_io(
    spec: SpcrNs16550Spec,
) -> Result<Option<(ProbedUart, SpcrNs16550Spec)>, OnProbeError> {
    let port = u16::try_from(spec.register_base).map_err(|_| {
        OnProbeError::other(format!(
            "SPCR has invalid NS16550 I/O base {:#x}",
            spec.register_base
        ))
    })?;
    let raw = erase_uart(serial_ns16550::Ns16550::new_port(port, spec.clock_hz));
    Ok(Some((raw, spec)))
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn uart_from_spcr_io(
    _spec: SpcrNs16550Spec,
) -> Result<Option<(ProbedUart, SpcrNs16550Spec)>, OnProbeError> {
    Err(OnProbeError::other(
        "SPCR NS16550 I/O ports are unsupported on this architecture",
    ))
}

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

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio_base = crate::mmio::iomap(base_reg.address as usize, mmio_size)?;
    let node = info.node.as_node();
    let reg_width = prop_u32(node, "reg-io-width").unwrap_or(1) as usize;
    let reg_shift = prop_u32(node, "reg-shift").map(|shift| 1usize << shift);
    let ns16550_width = reg_shift.unwrap_or(reg_width);
    let mut serial: Option<ProbedUart> = None;

    for compatible in node.compatibles() {
        if compatible == "snps,dw-apb-uart" {
            let default_clock = if node
                .compatibles()
                .any(|compatible| compatible == "rockchip,rk3588-uart")
            {
                serial_ns16550::dw_apb::RK3588_UART_CLOCK
            } else {
                serial_ns16550::dw_apb::SG2002_UART_CLOCK
            };
            let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(default_clock);
            let raw = serial_ns16550::DwApbUart::new_raw(mmio_base, clock_freq);
            serial = Some(erase_uart(raw));
            break;
        }

        if matches!(compatible, "ns16550a" | "ns16550") {
            let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(24_000_000);
            let raw = serial_ns16550::Ns16550::new_mmio(mmio_base, clock_freq, ns16550_width);
            serial = Some(erase_uart(raw));
            break;
        }
    }

    let serial = serial.ok_or(OnProbeError::NotMatch)?;
    let device_info = serial_device_info(&info, &base_reg);

    info!(
        "NS16550 serial@{:#x} registered successfully",
        serial.hardware.register_base
    );
    plat_dev.register(PlatformSerialDevice::new(
        serial,
        device_info.path,
        device_info.alias_index,
        device_info.paddr,
        device_info.irq,
    ));
    Ok(())
}

struct AcpiSerialResource {
    serial: ProbedUart,
    paddr: usize,
}

fn probe_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();

    info!("Probing ACPI NS16550 serial device: {}", info.path);
    let resource = if let Some(resource) = acpi_io_serial(info)? {
        resource
    } else {
        acpi_mmio_serial(info)?
    };
    let device_info = acpi_serial_device_info(info, resource.paddr);
    let plat_dev = probe.into_platform_device();

    info!(
        "ACPI NS16550 serial@{:#x} registered successfully",
        resource.paddr
    );
    plat_dev.register(PlatformSerialDevice::new(
        resource.serial,
        device_info.path,
        device_info.alias_index,
        device_info.paddr,
        device_info.irq,
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
    let serial = erase_uart(raw);
    Ok(Some(AcpiSerialResource {
        serial,
        paddr: usize::from(port),
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
    let serial = erase_uart(raw);
    Ok(AcpiSerialResource { serial, paddr })
}

#[cfg(test)]
mod tests {
    use rdrive::probe::acpi::AcpiResourceRange;

    use super::*;

    #[test]
    fn derives_runtime_ns16550_from_spcr_without_aml_namespace() {
        let console = AcpiSerialConsole {
            interface: AcpiSerialInterface::Uart16550,
            address_space: AcpiSerialAddressSpace::Io,
            registers: AcpiResourceRange {
                base: 0x3f8,
                size: 8,
            },
            access_size: 1,
            irq: Some(4),
            irq_route: Some(AcpiGsiRoute {
                gsi: 4,
                vector: 0x34,
                controller: rdrive::probe::acpi::AcpiGsiController::IoApic,
                controller_id: 2,
                controller_address: 0xfec0_0000,
                controller_input: 4,
                trigger: rdrive::probe::acpi::AcpiIrqTrigger::Edge,
                polarity: rdrive::probe::acpi::AcpiIrqPolarity::ActiveHigh,
            }),
            baud_rate: Some(115_200),
            clock_hz: None,
            namespace_path: None,
        };

        assert_eq!(
            spcr_ns16550_spec(&console),
            Some(SpcrNs16550Spec {
                address_space: AcpiSerialAddressSpace::Io,
                register_base: 0x3f8,
                register_size: 8,
                register_width: 1,
                clock_hz: ACPI_NS16550_CLOCK,
                initial_baudrate: 115_200,
                irq_route: console.irq_route.unwrap(),
            })
        );
    }

    #[test]
    fn rejects_spcr_fallback_without_interrupt_source() {
        let console = AcpiSerialConsole {
            interface: AcpiSerialInterface::Uart16550,
            address_space: AcpiSerialAddressSpace::Io,
            registers: AcpiResourceRange {
                base: 0x3f8,
                size: 8,
            },
            access_size: 1,
            irq: None,
            irq_route: None,
            baud_rate: Some(115_200),
            clock_hz: None,
            namespace_path: None,
        };

        assert_eq!(spcr_ns16550_spec(&console), None);
    }
}
