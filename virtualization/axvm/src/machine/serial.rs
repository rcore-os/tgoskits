//! Guest-visible serial resources selected by a machine profile.

use std::{string::String, vec::Vec};

#[cfg(target_arch = "loongarch64")]
use ax_std::os::arceos::driver as ax_driver;
use axdevice::{
    DeviceFirmwareBinding, DeviceFirmwareProperty, DeviceFirmwareSpec, ResolvedDeviceGraph,
};
use axdevice_base::AccessWidth;

use super::GuestMmioRegion;
use crate::{AxVmError, AxVmResult};

/// Guest-visible serial register model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialModel {
    /// 16550-compatible UART.
    Uart16550,
    /// Arm PrimeCell PL011 UART.
    Pl011,
}

/// Guest-visible serial register transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialTransport {
    /// x86 port I/O range.
    Port { base: u16, length: u16 },
    /// Memory-mapped register range.
    Mmio {
        base: usize,
        length: usize,
        /// Address stride expressed as a power-of-two register shift.
        register_shift: u8,
        /// Bus width used to access one register.
        register_width: AccessWidth,
    },
}

/// Machine-owned serial resources selected for one guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestSerialProfile {
    /// Guest-visible UART model.
    pub model: GuestSerialModel,
    /// Register transport and address range.
    pub transport: GuestSerialTransport,
    /// Virtual interrupt-controller input used by the UART.
    pub irq: usize,
    /// UART reference clock in hertz.
    pub clock_hz: u32,
}

/// Firmware identity retained when a host UART is replaced by a virtual UART.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestClockReference {
    /// Firmware phandle of the clock provider.
    pub provider_phandle: u32,
    /// Provider-specific clock specifier cells.
    pub specifier: Vec<u32>,
    /// Physical register windows owned by this provider.
    pub provider_regions: Vec<GuestMmioRegion>,
}

/// Firmware identity retained when a host UART is replaced by a virtual UART.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestSerialFdtIdentity {
    /// Absolute path of the firmware-selected UART node.
    pub node_path: String,
    /// UART node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Effective interrupt-controller phandle.
    pub interrupt_parent: u32,
    /// Raw firmware interrupt specifier.
    pub interrupt_specifier: Vec<u32>,
    /// Original `stdout-path` selector, including any line settings.
    pub stdout_path: String,
    /// Host clock dependencies that must remain protected after replacement.
    pub clock_references: Vec<GuestClockReference>,
}

/// Structured ACPI identity retained for a host-selected serial console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestSerialAcpiIdentity {
    /// Normalized namespace path when SPCR identifies an AML device.
    pub namespace_path: Option<String>,
    /// ACPI table that selected the console.
    pub source_table: [u8; 4],
}

/// Firmware identity retained by a virtual replacement of a host UART.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuestSerialFirmwareIdentity {
    Fdt(GuestSerialFdtIdentity),
    Acpi(GuestSerialAcpiIdentity),
}

impl GuestSerialFirmwareIdentity {
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    pub(crate) const fn fdt(&self) -> Option<&GuestSerialFdtIdentity> {
        match self {
            Self::Fdt(identity) => Some(identity),
            Self::Acpi(_) => None,
        }
    }

    pub(crate) fn binding(&self) -> DeviceFirmwareBinding {
        match self {
            Self::Fdt(identity) => DeviceFirmwareBinding::FdtNode(identity.node_path.clone()),
            Self::Acpi(identity) => identity
                .namespace_path
                .clone()
                .map(DeviceFirmwareBinding::AcpiDevice)
                .unwrap_or(DeviceFirmwareBinding::None),
        }
    }
}

/// Owned host-firmware serial snapshot used before the device graph is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSerialSnapshot {
    pub profile: GuestSerialProfile,
    pub identity: GuestSerialFirmwareIdentity,
}

#[cfg(any(
    target_arch = "loongarch64",
    all(target_arch = "x86_64", feature = "host-fs")
))]
pub(crate) fn host_serial_from_acpi(
    serial: ax_driver::probe::acpi::AcpiSerialConsole,
    fallback: GuestSerialProfile,
) -> AxVmResult<HostSerialSnapshot> {
    use ax_driver::probe::acpi::{AcpiSerialAddressSpace, AcpiSerialInterface};

    if serial.registers.size == 0 {
        return Err(AxVmError::invalid_config(
            "host SPCR serial register range is empty",
        ));
    }
    let model = match serial.interface {
        AcpiSerialInterface::Uart16550 => GuestSerialModel::Uart16550,
        AcpiSerialInterface::Pl011 => GuestSerialModel::Pl011,
    };
    let register_width = match serial.access_size {
        0 | 1 => AccessWidth::Byte,
        2 => AccessWidth::Word,
        3 => AccessWidth::Dword,
        4 => AccessWidth::Qword,
        value => {
            return Err(AxVmError::invalid_config(std::format!(
                "host SPCR serial access size {value} is invalid"
            )));
        }
    };
    let transport = match serial.address_space {
        AcpiSerialAddressSpace::Memory => GuestSerialTransport::Mmio {
            base: usize::try_from(serial.registers.base).map_err(|_| {
                AxVmError::invalid_config("host SPCR serial address exceeds the target width")
            })?,
            length: usize::try_from(serial.registers.size).map_err(|_| {
                AxVmError::invalid_config("host SPCR serial range exceeds the target width")
            })?,
            register_shift: 0,
            register_width: if model == GuestSerialModel::Pl011 {
                AccessWidth::Dword
            } else {
                register_width
            },
        },
        AcpiSerialAddressSpace::Io => GuestSerialTransport::Port {
            base: u16::try_from(serial.registers.base).map_err(|_| {
                AxVmError::invalid_config("host SPCR serial I/O port exceeds 16 bits")
            })?,
            length: u16::try_from(serial.registers.size).map_err(|_| {
                AxVmError::invalid_config("host SPCR serial I/O range exceeds 16 bits")
            })?,
        },
    };
    let irq = serial.irq.ok_or_else(|| {
        AxVmError::invalid_config("host SPCR selected a serial console without an interrupt")
    })?;
    Ok(HostSerialSnapshot {
        profile: GuestSerialProfile {
            model,
            transport,
            irq: usize::try_from(irq)
                .map_err(|_| AxVmError::invalid_config("host SPCR IRQ exceeds usize"))?,
            clock_hz: serial.clock_hz.unwrap_or(fallback.clock_hz),
        },
        identity: GuestSerialFirmwareIdentity::Acpi(GuestSerialAcpiIdentity {
            namespace_path: serial.namespace_path,
            source_table: *b"SPCR",
        }),
    })
}

/// Interrupt encoding used when the common FDT pipeline describes a UART.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestSerialFdtInterrupt {
    /// Arm GIC SPI tuple.
    GicSpi,
    /// RISC-V PLIC source number.
    PlicSource,
}

/// One serial node resolved from the same graph used to build its runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedSerialDevice {
    id: String,
    profile: GuestSerialProfile,
    firmware_binding: DeviceFirmwareBinding,
}

impl ResolvedSerialDevice {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) const fn profile(&self) -> GuestSerialProfile {
        self.profile
    }

    pub(crate) const fn firmware_binding(&self) -> &DeviceFirmwareBinding {
        &self.firmware_binding
    }
}

/// Resolves conventional serial firmware metadata against planned resources.
pub(crate) fn resolved_serial_devices(
    graph: &ResolvedDeviceGraph,
) -> AxVmResult<Vec<ResolvedSerialDevice>> {
    graph
        .nodes()
        .filter_map(|node| {
            let firmware = node.firmware();
            serial_model(&firmware).map(|model| (node, firmware, model))
        })
        .map(|(node, firmware, model)| {
            let registers = single_slot(&firmware, firmware.register_slots(), "register")?;
            let interrupt = single_slot(&firmware, firmware.interrupt_slots(), "interrupt")?;
            let resources = graph.resources_for(node.id())?;
            let transport = if let Some((_, base, length)) =
                resources.pio_ranges().find(|(slot, ..)| *slot == registers)
            {
                GuestSerialTransport::Port { base, length }
            } else {
                let (base, length) = resources.mmio(registers)?;
                GuestSerialTransport::Mmio {
                    base: usize::try_from(base)
                        .map_err(|_| serial_range_error(node.id().as_str()))?,
                    length: usize::try_from(length)
                        .map_err(|_| serial_range_error(node.id().as_str()))?,
                    register_shift: u8::try_from(u32_property(&firmware, "reg-shift").unwrap_or(0))
                        .map_err(|_| serial_property_error(node.id().as_str(), "reg-shift"))?,
                    register_width: AccessWidth::try_from(
                        usize::try_from(u32_property(&firmware, "reg-io-width").unwrap_or(1))
                            .map_err(|_| {
                                serial_property_error(node.id().as_str(), "reg-io-width")
                            })?,
                    )
                    .map_err(|()| serial_property_error(node.id().as_str(), "reg-io-width"))?,
                }
            };
            let irq = resources.wired_irq(interrupt)?.input().value();
            Ok(ResolvedSerialDevice {
                id: node.id().as_str().into(),
                profile: GuestSerialProfile {
                    model,
                    transport,
                    irq,
                    clock_hz: u32_property(&firmware, "clock-frequency").unwrap_or(1_843_200),
                },
                firmware_binding: node.firmware_binding().clone(),
            })
        })
        .collect()
}

fn serial_model(firmware: &DeviceFirmwareSpec) -> Option<GuestSerialModel> {
    if firmware
        .compatible()
        .iter()
        .any(|compatible| compatible == "arm,pl011")
    {
        Some(GuestSerialModel::Pl011)
    } else if firmware
        .compatible()
        .iter()
        .any(|compatible| matches!(compatible.as_str(), "ns16550" | "ns16550a"))
    {
        Some(GuestSerialModel::Uart16550)
    } else {
        None
    }
}

fn single_slot<'a>(
    firmware: &DeviceFirmwareSpec,
    slots: &'a [axdevice::ResourceSlot],
    kind: &'static str,
) -> AxVmResult<&'a axdevice::ResourceSlot> {
    let [slot] = slots else {
        return Err(AxVmError::invalid_config(std::format!(
            "serial firmware model {:?} must declare exactly one {kind} slot",
            firmware.node_name()
        )));
    };
    Ok(slot)
}

fn u32_property(firmware: &DeviceFirmwareSpec, name: &str) -> Option<u32> {
    firmware
        .properties()
        .iter()
        .find_map(|property| match property {
            DeviceFirmwareProperty::U32 {
                name: property_name,
                value,
            } if property_name == name => Some(*value),
            _ => None,
        })
}

fn serial_range_error(device: &str) -> AxVmError {
    AxVmError::invalid_config(std::format!(
        "resolved serial range for {device} exceeds the target address width"
    ))
}

fn serial_property_error(device: &str, property: &'static str) -> AxVmError {
    AxVmError::invalid_config(std::format!(
        "resolved serial {device} has invalid {property}"
    ))
}

#[cfg(all(test, target_arch = "x86_64", feature = "host-fs"))]
mod tests;
