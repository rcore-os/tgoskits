//! Serial-device facts derived from the resolved x86 device graph.

use std::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use axdevice::*;

use super::config::X86FirmwarePlanError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum X86SerialRegisters {
    Port { base: u16, size: u16 },
    Mmio { base: u32, size: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86SerialPlan {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) namespace_path: Option<String>,
    pub(super) hid: String,
    pub(super) interface_type: u8,
    pub(super) registers: X86SerialRegisters,
    pub(super) spcr_gas_layout: Option<(u8, u8)>,
    pub(super) irq: u32,
    pub(super) clock_hz: u32,
}

pub(super) fn x86_serial_plans(
    graph: &ResolvedDeviceGraph,
) -> Result<Vec<X86SerialPlan>, X86FirmwarePlanError> {
    let mut serials = crate::machine::resolved_serial_devices(graph).map_err(|error| {
        X86FirmwarePlanError::InvalidValue {
            field: "serial device graph",
            value: error.to_string(),
        }
    })?;
    serials.sort_by_key(|serial| (serial.id() != "console0", serial.id().to_string()));
    if serials
        .first()
        .is_none_or(|serial| serial.id() != "console0")
    {
        return Err(X86FirmwarePlanError::MissingDevice {
            node_id: "console0",
        });
    }
    serials
        .into_iter()
        .enumerate()
        .map(|(index, serial)| serial_plan(index, serial))
        .collect()
}

fn serial_plan(
    index: usize,
    serial: crate::machine::ResolvedSerialDevice,
) -> Result<X86SerialPlan, X86FirmwarePlanError> {
    let id = serial.id().to_string();
    let profile = serial.profile();
    let registers = match profile.transport {
        crate::machine::GuestSerialTransport::Port { base, length } => {
            X86SerialRegisters::Port { base, size: length }
        }
        crate::machine::GuestSerialTransport::Mmio { base, length, .. } => {
            X86SerialRegisters::Mmio {
                base: u32::try_from(base)
                    .map_err(|_| invalid_serial_value("serial MMIO base", base))?,
                size: u32::try_from(length)
                    .map_err(|_| invalid_serial_value("serial MMIO size", length))?,
            }
        }
    };
    let (generated_name, hid, interface_type, spcr_gas_layout) =
        match (profile.model, profile.transport) {
            (
                crate::machine::GuestSerialModel::Uart16550,
                crate::machine::GuestSerialTransport::Mmio {
                    register_shift,
                    register_width,
                    ..
                },
            ) => (
                format!("COM{}", index + 1),
                "PNP0501".into(),
                0x12,
                crate::machine::spcr_16550_mmio_fields(register_shift, register_width),
            ),
            (crate::machine::GuestSerialModel::Uart16550, _) => (
                format!("COM{}", index + 1),
                "PNP0501".into(),
                0,
                Some((8, 1)),
            ),
            (crate::machine::GuestSerialModel::Pl011, _) => {
                (format!("URT{index}"), "ARMH0011".into(), 3, Some((32, 3)))
            }
        };
    let namespace_path = match serial.firmware_binding() {
        DeviceFirmwareBinding::AcpiDevice(path) => Some(path.clone()),
        _ => None,
    };
    let name = namespace_path
        .as_deref()
        .and_then(|path| path.rsplit(['.', '\\']).find(|part| !part.is_empty()))
        .unwrap_or(&generated_name)
        .into();
    Ok(X86SerialPlan {
        id,
        name,
        namespace_path,
        hid,
        interface_type,
        registers,
        spcr_gas_layout,
        irq: u32::try_from(profile.irq)
            .map_err(|_| invalid_serial_value("serial IRQ", profile.irq))?,
        clock_hz: profile.clock_hz,
    })
}

fn invalid_serial_value(
    field: &'static str,
    value: impl core::fmt::LowerHex,
) -> X86FirmwarePlanError {
    X86FirmwarePlanError::InvalidValue {
        field,
        value: format!("{value:#x}"),
    }
}
