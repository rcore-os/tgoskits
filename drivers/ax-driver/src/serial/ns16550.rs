use alloc::format;

use fdt_edit::RegFixed;
use log::{info, warn};
use rdrive::{
    probe::{
        OnProbeError,
        acpi::{AcpiId, AcpiInfo, AcpiResourceAddress, ProbeAcpi, Spcr16550Mmio},
    },
    register::{FdtInfo, ProbeFdt},
};
use some_serial::ns16550 as serial_ns16550;

#[cfg(target_arch = "x86_64")]
use super::BindingIrq;
use super::{
    PlatformSerialDevice, ProbedUart, SerialRegisterAddress, acpi_serial_device_info, erase_uart,
    prop_u32, serial_device_info,
};

const ACPI_NS16550_IDS: &[AcpiId] = &[
    AcpiId {
        hid: "PNP0501",
        cids: &[],
    },
    AcpiId {
        hid: "PNP0500",
        cids: &[],
    },
];
const ACPI_NS16550_CLOCK: u32 = 1_843_200;
const ACPI_NS16550_REG_WIDTH: usize = 1;
const LEGACY_FDT_NS16550_CLOCK: u32 = 24_000_000;
#[cfg(target_arch = "x86_64")]
const LEGACY_COM1_PORT: u16 = 0x3f8;
#[cfg(target_arch = "x86_64")]
const LEGACY_COM1_ISA_IRQ: u8 = 4;

model_register!(
    name: "NS16550 serial",
    // Indexed UART clock providers are probed before the PostKernel consumers.
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "snps,dw-apb-uart",
                "spacemit,k1-uart",
                "spacemit,k3-uart",
                "intel,xscale-uart",
                "ns16550a",
                "ns16550",
            ],
            on_probe: probe
        },
        ProbeKind::Acpi {
            ids: ACPI_NS16550_IDS,
            on_probe: probe_acpi
        },
        #[cfg(target_arch = "x86_64")]
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_legacy_com1
        },
    ],
);

#[cfg(target_arch = "x86_64")]
fn probe_legacy_com1(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let base = u64::from(LEGACY_COM1_PORT);
    if probe
        .info()
        .root
        .namespace_claims_io_range(ACPI_NS16550_IDS, base..base + 8)
        .map_err(|error| {
            OnProbeError::other(format!("cannot inspect ACPI COM1 resources: {error:?}"))
        })?
    {
        return Err(OnProbeError::NotMatch);
    }
    let route = probe
        .info()
        .root
        .routing()
        .resolve_isa_irq(LEGACY_COM1_ISA_IRQ)
        .ok_or_else(|| OnProbeError::other("legacy COM1 IRQ4 has no ACPI I/O APIC route"))?;
    let serial = erase_uart(serial_ns16550::Ns16550::new_port(
        LEGACY_COM1_PORT,
        ACPI_NS16550_CLOCK,
    ));

    info!("Legacy x86 COM1 fallback registered successfully");
    probe.register_root_resource_device(
        AcpiResourceAddress::io(u64::from(LEGACY_COM1_PORT)),
        PlatformSerialDevice::new(
            serial,
            "legacy/com1".into(),
            Some(0),
            SerialRegisterAddress::IoPort(LEGACY_COM1_PORT),
            Some(BindingIrq::from(route)),
        ),
    );
    Ok(())
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();

    info!("Probing NS16550 serial device: {}", info.node.name());
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;

    let paddr = usize::try_from(base_reg.address)
        .map_err(|_| OnProbeError::other("NS16550 MMIO address exceeds address width"))?;
    let node = info.node.as_node();
    let mut serial = None;

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
            let mmio_size = usize::try_from(base_reg.size.unwrap_or(0x1000)).map_err(|_| {
                OnProbeError::other("DesignWare UART MMIO size exceeds address width")
            })?;
            let mmio_base = crate::mmio::iomap(paddr, mmio_size)?;
            let raw = serial_ns16550::DwApbUart::new_raw(mmio_base, clock_freq);
            serial = Some((erase_uart(raw), paddr));
            break;
        }

        if matches!(
            compatible,
            "spacemit,k1-uart" | "spacemit,k3-uart" | "intel,xscale-uart" | "ns16550a" | "ns16550"
        ) {
            let xscale = node.compatibles().any(|compatible| {
                matches!(
                    compatible,
                    "spacemit,k1-uart" | "spacemit,k3-uart" | "intel,xscale-uart"
                )
            });
            let variant = if xscale {
                serial_ns16550::UartVariant::XScale
            } else {
                serial_ns16550::UartVariant::Standard
            };
            let clock_freq =
                ns16550_clock_frequency(node, xscale, || fdt_uart_clock_rate(&info, xscale))?;
            let layout = ns16550_register_layout(node, variant)?;
            let (paddr, mmio_size) = ns16550_register_range(node, paddr, &base_reg)
                .ok_or_else(|| OnProbeError::other("invalid NS16550 MMIO resource range"))?;
            let raw = map_ns16550(paddr, mmio_size, clock_freq, layout, variant)?;
            let mut probed = erase_uart(raw);
            probed.hardware.initial_baudrate =
                initial_baudrate(node, clock_freq, probed.hardware.initial_baudrate);
            serial = Some((probed, paddr));
            break;
        }
    }

    let (serial, paddr) = serial.ok_or(OnProbeError::NotMatch)?;
    let device_info = serial_device_info(&info, &base_reg);

    info!(
        "NS16550 serial@{:#x} registered successfully",
        serial.hardware.register_base
    );
    plat_dev.register(PlatformSerialDevice::new(
        serial,
        device_info.path,
        device_info.alias_index,
        SerialRegisterAddress::Mmio(paddr),
        device_info.irq,
    ));
    Ok(())
}

fn ns16550_register_range(
    node: &fdt_edit::Node,
    paddr: usize,
    reg: &RegFixed,
) -> Option<(usize, usize)> {
    let size = usize::try_from(reg.size?).ok()?;
    let offset = match node.get_property("reg-offset") {
        Some(prop) => usize::try_from(prop.get_u32()?).ok()?,
        None => 0,
    };
    let size = size.checked_sub(offset).filter(|size| *size != 0)?;
    Some((paddr.checked_add(offset)?, size))
}

fn ns16550_register_layout(
    node: &fdt_edit::Node,
    variant: serial_ns16550::UartVariant,
) -> Result<serial_ns16550::MmioLayout, OnProbeError> {
    let read_property = |name| {
        node.get_property(name)
            .map(|prop| {
                prop.get_u32().ok_or_else(|| {
                    OnProbeError::other(format!("NS16550 {name} must contain one u32"))
                })
            })
            .transpose()
    };
    variant
        .mmio_layout(read_property("reg-shift")?, read_property("reg-io-width")?)
        .map_err(|error| OnProbeError::other(format!("invalid NS16550 MMIO layout: {error}")))
}

fn ns16550_clock_frequency(
    node: &fdt_edit::Node,
    xscale: bool,
    core_clock_rate: impl FnOnce() -> Result<Option<u64>, OnProbeError>,
) -> Result<u32, OnProbeError> {
    if let Some(rate) = prop_u32(node, "clock-frequency") {
        return Ok(rate);
    }

    if node.get_property("clocks").is_some() {
        // A declared but unavailable clock must preserve the firmware divisor.
        return u32::try_from(core_clock_rate()?.unwrap_or(0))
            .map_err(|_| OnProbeError::other("NS16550 core clock rate exceeds u32"));
    }

    if xscale {
        return Ok(0);
    }
    // Legacy nodes used this assumed clock when decoding and configuring
    // the firmware-derived startup baud rate.
    Ok(LEGACY_FDT_NS16550_CLOCK)
}

fn initial_baudrate(node: &fdt_edit::Node, clock_freq: u32, divisor_baudrate: u32) -> u32 {
    if clock_freq == 0 {
        // The nominal firmware baud rate does not provide a programmable input clock.
        prop_u32(node, "current-speed").unwrap_or(divisor_baudrate)
    } else {
        divisor_baudrate
    }
}

fn fdt_uart_clock_rate(info: &FdtInfo<'_>, xscale: bool) -> Result<Option<u64>, OnProbeError> {
    let clocks = info.clocks()?;
    let fallback = if xscale {
        match clocks.as_slice() {
            [clock] if !matches!(clock.name.as_deref(), Some("bus" | "reg")) => Some(clock),
            _ => None,
        }
    } else {
        clocks.first()
    };
    let Some(clock) = clocks
        .iter()
        .find(|clock| clock.name.as_deref() == Some("core"))
        .or(fallback)
    else {
        return Ok(None);
    };

    if clock.cells == 0 {
        let rate = info.get_by_phandle(clock.phandle).and_then(|provider| {
            let node = provider.as_node();
            node.compatibles()
                .any(|compatible| compatible == "fixed-clock")
                .then(|| prop_u32(node, "clock-frequency"))
                .flatten()
        });
        return Ok(rate.map(u64::from));
    }

    match info
        .optional_clock_line(clock)
        .and_then(|line| line.map(|line| line.rate()).transpose())
    {
        Ok(rate) => Ok(rate),
        Err(error) => {
            warn!("{}: UART clock rate unavailable: {error}", info.node.name());
            Ok(None)
        }
    }
}

struct AcpiSerialResource {
    serial: ProbedUart,
    address: SerialRegisterAddress,
}

fn probe_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();

    info!("Probing ACPI NS16550 serial device: {}", info.path);
    let spcr = info
        .spcr_16550_mmio()
        .map_err(|error| OnProbeError::other(format!("cannot read SPCR UART settings: {error}")))?;
    let resource = if let Some(spcr) = spcr {
        acpi_mmio_serial(info, Some(spcr))?
    } else if let Some(resource) = acpi_io_serial(info)? {
        resource
    } else {
        acpi_mmio_serial(info, None)?
    };
    let device_info = acpi_serial_device_info(info, resource.address.base());

    info!(
        "ACPI NS16550 serial@{:#x} registered successfully",
        resource.address.base()
    );
    let device = PlatformSerialDevice::new(
        resource.serial,
        device_info.path,
        device_info.alias_index,
        resource.address,
        device_info.irq,
    );
    if let Some(spcr) = spcr {
        probe.register_device_at(AcpiResourceAddress::memory(spcr.registers.base), device);
    } else {
        probe.into_platform_device().register(device);
    }
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
        address: SerialRegisterAddress::IoPort(port),
    }))
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn acpi_io_serial(_info: &AcpiInfo<'_>) -> Result<Option<AcpiSerialResource>, OnProbeError> {
    Ok(None)
}

fn acpi_mmio_serial(
    info: &AcpiInfo<'_>,
    spcr: Option<Spcr16550Mmio>,
) -> Result<AcpiSerialResource, OnProbeError> {
    let range = spcr
        .map(|console| console.registers)
        .or_else(|| info.memory_ranges().first().copied())
        .ok_or_else(|| {
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
    let mmio_size = usize::try_from(range.size)
        .map_err(|_| OnProbeError::other("invalid ACPI NS16550 MMIO resource size"))?;
    let (layout, clock) = acpi_mmio_parameters(spcr)?;
    let raw = map_ns16550(
        paddr,
        mmio_size,
        clock,
        layout,
        serial_ns16550::UartVariant::Standard,
    )?;
    let mut serial = erase_uart(raw);
    if clock == 0 {
        serial.hardware.initial_baudrate = spcr.and_then(|console| console.baud_rate).unwrap_or(0);
    }
    Ok(AcpiSerialResource {
        serial,
        address: SerialRegisterAddress::Mmio(paddr),
    })
}

fn acpi_mmio_parameters(
    spcr: Option<Spcr16550Mmio>,
) -> Result<(serial_ns16550::MmioLayout, u32), OnProbeError> {
    let (stride, width, clock) = spcr.map_or(
        (
            ACPI_NS16550_REG_WIDTH,
            ACPI_NS16550_REG_WIDTH,
            ACPI_NS16550_CLOCK,
        ),
        |spcr| {
            (
                spcr.register_stride,
                spcr.access_width,
                spcr.clock_hz.unwrap_or(0),
            )
        },
    );
    let layout = serial_ns16550::MmioLayout::new(stride, width)
        .map_err(|error| OnProbeError::other(format!("invalid NS16550 MMIO layout: {error}")))?;
    Ok((layout, clock))
}

fn map_ns16550(
    paddr: usize,
    mmio_size: usize,
    clock: u32,
    layout: serial_ns16550::MmioLayout,
    variant: serial_ns16550::UartVariant,
) -> Result<serial_ns16550::Ns16550<serial_ns16550::Mmio>, OnProbeError> {
    if layout.required_len() > mmio_size || paddr.checked_add(mmio_size).is_none() {
        return Err(OnProbeError::other("invalid NS16550 MMIO resource range"));
    }

    let mmio_base = crate::mmio::iomap(paddr, mmio_size)?;
    // SAFETY: the firmware resource describes this UART and iomap retains the
    // register mapping. The runtime serial device coordinates endpoint access.
    unsafe { serial_ns16550::Ns16550::new_mmio(mmio_base, mmio_size, clock, layout, variant) }
        .map_err(|error| OnProbeError::other(format!("invalid NS16550 MMIO layout: {error}")))
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use fdt_edit::{Node, Property};
    use some_serial::UartPort;

    use super::*;

    #[test]
    fn acpi_runtime_uart_masks_ier_using_spcr_mmio_layout() {
        use rdrive::probe::acpi::AcpiResourceRange;

        let resources = AcpiResourceRange {
            base: 0x2000,
            size: 0x100,
        };
        // A four-byte stride can use 32-bit or byte accesses.
        for (access_width, expected_ier) in [(4, 0), (1, 0xabcd_ef00)] {
            let mut registers = [0_u32; 8];
            registers[1] = 0xabcd_ef0f;
            let base = NonNull::new(registers.as_mut_ptr().cast()).unwrap();
            let (layout, clock) = acpi_mmio_parameters(Some(Spcr16550Mmio {
                registers: resources,
                register_stride: 4,
                access_width,
                clock_hz: Some(24_000_000),
                baud_rate: None,
            }))
            .unwrap();
            // SAFETY: the aligned backing array is exclusive to this UART fixture.
            let mut port = unsafe {
                serial_ns16550::Ns16550::new_mmio(
                    base,
                    size_of_val(&registers),
                    clock,
                    layout,
                    serial_ns16550::UartVariant::Standard,
                )
            }
            .unwrap();

            port.mask_all();
            assert_eq!(registers[1], expected_ier, "IER is at byte offset four");
            assert_eq!(clock, 24_000_000);
        }
        assert_eq!(
            acpi_mmio_parameters(Some(Spcr16550Mmio {
                registers: resources,
                register_stride: 4,
                access_width: 4,
                clock_hz: None,
                baud_rate: Some(115_200),
            }))
            .unwrap(),
            (serial_ns16550::MmioLayout::new(4, 4).unwrap(), 0),
        );
        assert_eq!(
            acpi_mmio_parameters(None).unwrap(),
            (serial_ns16550::MmioLayout::new(1, 1).unwrap(), 1_843_200),
        );
    }

    #[test]
    fn fdt_uart_register_layout_preserves_legacy_spacing_without_reg_shift() {
        let mut node = Node::new("serial@1000");
        node.set_property(Property::new("reg-io-width", 4_u32.to_be_bytes().to_vec()));
        let variant = serial_ns16550::UartVariant::Standard;

        assert_eq!(
            ns16550_register_layout(&node, variant).unwrap(),
            serial_ns16550::MmioLayout::new(4, 4).unwrap(),
        );

        node.set_property(Property::new("reg-shift", 3_u32.to_be_bytes().to_vec()));
        assert_eq!(
            ns16550_register_layout(&node, variant).unwrap(),
            serial_ns16550::MmioLayout::new(8, 4).unwrap(),
        );

        node.set_property(Property::new("reg-io-width", alloc::vec![4]));
        assert!(ns16550_register_layout(&node, variant).is_err());
    }

    #[test]
    fn fdt_uart_register_range_applies_offset_within_resource() {
        let mut node = Node::new("serial@1000");
        let reg = RegFixed {
            address: 0x1000,
            child_bus_address: 0x1000,
            size: Some(0x100),
        };
        let paddr = 0x1000;
        assert_eq!(
            ns16550_register_range(&node, paddr, &reg),
            Some((0x1000, 0x100))
        );

        node.set_property(Property::new("reg-offset", 0x40_u32.to_be_bytes().to_vec()));
        assert_eq!(
            ns16550_register_range(&node, paddr, &reg),
            Some((0x1040, 0xc0))
        );

        node.set_property(Property::new(
            "reg-offset",
            0x100_u32.to_be_bytes().to_vec(),
        ));
        assert_eq!(ns16550_register_range(&node, paddr, &reg), None);
    }

    #[test]
    fn fdt_uart_clock_uses_firmware_rate_or_preserves_unknown_clock() {
        let mut node = Node::new("serial@1000");
        assert_eq!(
            ns16550_clock_frequency(&node, false, || panic!("clock is not declared")).unwrap(),
            LEGACY_FDT_NS16550_CLOCK
        );

        node.set_property(Property::new("clocks", [0, 0, 0, 1, 0, 0, 0, 0].to_vec()));
        node.set_property(Property::new(
            "current-speed",
            115_200_u32.to_be_bytes().to_vec(),
        ));
        let unresolved = ns16550_clock_frequency(&node, false, || Ok(None)).unwrap();
        assert_eq!(unresolved, 0);
        assert_eq!(initial_baudrate(&node, unresolved, 0), 115_200);

        let resolved = ns16550_clock_frequency(&node, false, || Ok(Some(57_600_000))).unwrap();
        assert_eq!(resolved, 57_600_000);
        assert_eq!(initial_baudrate(&node, resolved, 57_600), 57_600);

        node.set_property(Property::new(
            "clock-frequency",
            48_000_000_u32.to_be_bytes().to_vec(),
        ));
        assert_eq!(
            ns16550_clock_frequency(&node, false, || panic!("clock-frequency takes precedence"))
                .unwrap(),
            48_000_000
        );
    }
}
