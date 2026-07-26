use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use fdt_edit::{Fdt, RegFixed};
use log::warn;
use rdif_serial::{SplitUart, UartInfo, UartIrq, UartParts, UartPort};
use rdrive::{Device, DeviceId, DriverGeneric, probe::acpi::AcpiInfo, register::FdtInfo};

mod ns16550;
mod pl011;
mod rockchip_fiq;

use crate::{BindingInfo, BindingIrq, binding_info_from_acpi, binding_info_from_fdt};

type ErasedUartParts = UartParts<Box<dyn UartPort>, Box<dyn UartIrq>>;

struct ProbedUart {
    hardware: UartInfo,
    parts: ErasedUartParts,
}

struct PlatformSerialDevice {
    name: String,
    firmware_path: String,
    alias_index: Option<usize>,
    paddr: usize,
    initial_baudrate: u32,
    irq: Option<BindingIrq>,
    parts: Option<ErasedUartParts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialDeviceInfo {
    pub name: String,
    pub device_id: DeviceId,
    pub firmware_path: String,
    pub alias_index: Option<usize>,
    pub paddr: usize,
    pub initial_baudrate: u32,
    pub irq: Option<BindingIrq>,
}

pub struct SerialDevice {
    pub info: SerialDeviceInfo,
    pub port: Box<dyn UartPort>,
    pub irq: Box<dyn UartIrq>,
}

impl PlatformSerialDevice {
    fn new(
        probe: ProbedUart,
        firmware_path: String,
        alias_index: Option<usize>,
        paddr: usize,
        irq: Option<BindingIrq>,
    ) -> Self {
        Self {
            name: probe.hardware.name.into(),
            firmware_path,
            alias_index,
            paddr,
            initial_baudrate: probe.hardware.initial_baudrate,
            irq,
            parts: Some(probe.parts),
        }
    }
}

impl DriverGeneric for PlatformSerialDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

fn erase_uart(raw: impl SplitUart) -> ProbedUart {
    let hardware = raw.runtime_info();
    let UartParts { port, irq } = raw.split();
    ProbedUart {
        hardware,
        parts: UartParts::new(Box::new(port), Box::new(irq)),
    }
}

impl TryFrom<Device<PlatformSerialDevice>> for SerialDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformSerialDevice>) -> Result<Self, Self::Error> {
        let device_id = base.descriptor().device_id();
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let parts = dev.parts.take().ok_or(AxError::BadState)?;
        Ok(Self {
            info: SerialDeviceInfo {
                name: dev.name.clone(),
                device_id,
                firmware_path: dev.firmware_path.clone(),
                alias_index: dev.alias_index,
                paddr: dev.paddr,
                initial_baudrate: dev.initial_baudrate,
                irq: dev.irq.clone(),
            },
            port: parts.port,
            irq: parts.irq,
        })
    }
}

pub fn take_serial_devices() -> Vec<SerialDevice> {
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; no serial devices available");
        return Vec::new();
    }

    rdrive::get_list::<PlatformSerialDevice>()
        .into_iter()
        .filter_map(|dev| match SerialDevice::try_from(dev) {
            Ok(serial) => Some(serial),
            Err(err) => {
                warn!("failed to take serial device: {err:?}");
                None
            }
        })
        .collect()
}

struct SerialFirmwareInfo {
    path: String,
    alias_index: Option<usize>,
    paddr: usize,
    irq: Option<BindingIrq>,
}

fn serial_device_info(info: &FdtInfo<'_>, base_reg: &RegFixed) -> SerialFirmwareInfo {
    let path = info.node.path();
    let alias_index = rdrive::with_fdt(|fdt| serial_alias_index(fdt, &path)).flatten();
    let irq = serial_binding_info(info, &path).irq_cloned();
    SerialFirmwareInfo {
        path,
        alias_index,
        paddr: base_reg.address as usize,
        irq,
    }
}

fn acpi_serial_device_info(info: &AcpiInfo<'_>, paddr: usize) -> SerialFirmwareInfo {
    let binding_info = acpi_serial_binding_info(info);
    let irq = binding_info.irq_cloned();
    SerialFirmwareInfo {
        path: info.path.into(),
        alias_index: None,
        paddr,
        irq,
    }
}

fn serial_binding_info(info: &FdtInfo<'_>, fdt_path: &str) -> BindingInfo {
    binding_info_from_fdt(info).unwrap_or_else(|err| {
        warn!("failed to resolve serial IRQ for {fdt_path}: {err:?}");
        BindingInfo::empty()
    })
}

fn acpi_serial_binding_info(info: &AcpiInfo<'_>) -> BindingInfo {
    binding_info_from_acpi(info).unwrap_or_else(|err| {
        warn!(
            "failed to resolve ACPI serial IRQ for {}: {err:?}",
            info.path
        );
        BindingInfo::empty()
    })
}

fn serial_alias_index(fdt: &Fdt, node_path: &str) -> Option<usize> {
    let aliases = fdt.get_by_path("/aliases")?;
    aliases
        .as_node()
        .properties()
        .iter()
        .filter_map(|prop| {
            let index = prop.name().strip_prefix("serial")?.parse::<usize>().ok()?;
            let path = prop.as_str()?;
            (path == node_path).then_some(index)
        })
        .next()
}

fn prop_u32(node: &fdt_edit::Node, name: &str) -> Option<u32> {
    node.get_property(name).and_then(|prop| prop.get_u32())
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn resolves_serial_alias_index_by_node_path() {
        let fdt = minimal_serial_alias_fdt();

        assert_eq!(serial_alias_index(&fdt, "/soc/uart@1000"), Some(0));
        assert_eq!(serial_alias_index(&fdt, "/soc/uart@2000"), Some(2));
        assert_eq!(serial_alias_index(&fdt, "/soc/uart@3000"), None);
    }

    fn minimal_serial_alias_fdt() -> Fdt {
        minimal_serial_alias_fdt_with_root_compatible(&[])
    }

    fn minimal_serial_alias_fdt_with_root_compatible(compatibles: &[&str]) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        if !compatibles.is_empty() {
            fdt.node_mut(root)
                .unwrap()
                .set_property(prop_strs("compatible", compatibles));
        }
        let aliases = fdt.add_node(root, Node::new("aliases"));
        fdt.node_mut(aliases)
            .unwrap()
            .set_property(prop_str("serial0", "/soc/uart@1000"));
        fdt.node_mut(aliases)
            .unwrap()
            .set_property(prop_str("serial2", "/soc/uart@2000"));

        let soc = fdt.add_node(root, Node::new("soc"));
        fdt.add_node(soc, Node::new("uart@1000"));
        fdt.add_node(soc, Node::new("uart@2000"));
        fdt
    }

    fn prop_str(name: &str, value: &str) -> Property {
        let mut data = Vec::new();
        data.extend_from_slice(value.as_bytes());
        data.push(0);
        Property::new(name, data)
    }

    fn prop_strs(name: &str, values: &[&str]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        Property::new(name, data)
    }
}
