use alloc::{string::String, vec::Vec};

use ax_errno::AxError;
use fdt_edit::{Fdt, RegFixed};
use log::warn;
pub use rdif_serial::{
    Config, ConfigError, RxFlag, RxItem, SerialCounters, SerialIrqOutcome, SerialSoftWork,
};
use rdrive::{Device, DeviceId, DriverGeneric, probe::acpi::AcpiInfo, register::FdtInfo};

mod ns16550;
mod pl011;
mod rockchip_fiq;
mod runtime;

pub use runtime::SerialPort;

use crate::{BindingInfo, binding_info_from_acpi, binding_info_from_fdt};

struct PlatformSerialDevice {
    name: String,
    info: SerialDeviceInfo,
    port: Option<SerialPort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialDeviceInfo {
    pub fdt_path: String,
    pub alias_index: Option<usize>,
    pub paddr: usize,
    pub mapped_base: usize,
    pub baudrate: u32,
    pub irq_num: Option<usize>,
    pub binding_info: BindingInfo,
}

pub struct SerialDevice {
    name: String,
    rdrive_device_id: DeviceId,
    info: SerialDeviceInfo,
    port: SerialPort,
}

impl PlatformSerialDevice {
    fn new(name: String, info: SerialDeviceInfo, port: SerialPort) -> Self {
        Self {
            name,
            info,
            port: Some(port),
        }
    }
}

impl DriverGeneric for PlatformSerialDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl SerialDevice {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn info(&self) -> &SerialDeviceInfo {
        &self.info
    }

    pub fn rdrive_device_id(&self) -> DeviceId {
        self.rdrive_device_id
    }

    pub fn fdt_path(&self) -> &str {
        &self.info.fdt_path
    }

    pub fn alias_index(&self) -> Option<usize> {
        self.info.alias_index
    }

    pub fn paddr(&self) -> usize {
        self.info.paddr
    }

    pub fn mapped_base(&self) -> usize {
        self.info.mapped_base
    }

    pub fn baudrate(&self) -> u32 {
        self.port.baudrate()
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num
    }

    pub fn set_config(&self, config: &Config) -> Result<(), ConfigError> {
        self.port.set_config(config)
    }

    pub fn set_baudrate(&self, baudrate: u32) -> Result<(), ConfigError> {
        self.port.set_config(&Config::new().baudrate(baudrate))
    }

    pub fn into_port(self) -> SerialPort {
        self.port
    }
}

impl TryFrom<Device<PlatformSerialDevice>> for SerialDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformSerialDevice>) -> Result<Self, Self::Error> {
        let rdrive_device_id = base.descriptor().device_id();
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let info = dev.info.clone();
        let port = dev.port.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            rdrive_device_id,
            info,
            port,
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

fn serial_device_info(
    info: &FdtInfo<'_>,
    base_reg: &RegFixed,
    mapped_base: usize,
    baudrate: u32,
) -> SerialDeviceInfo {
    let fdt_path = info.node.path();
    let alias_index = rdrive::with_fdt(|fdt| serial_alias_index(fdt, &fdt_path)).flatten();
    let binding_info = serial_binding_info(info, &fdt_path);
    SerialDeviceInfo {
        fdt_path,
        alias_index,
        paddr: base_reg.address as usize,
        mapped_base,
        baudrate,
        irq_num: binding_info.irq_num(),
        binding_info,
    }
}

fn acpi_serial_device_info(
    info: &AcpiInfo<'_>,
    paddr: usize,
    mapped_base: usize,
    baudrate: u32,
) -> SerialDeviceInfo {
    let binding_info = acpi_serial_binding_info(info);
    SerialDeviceInfo {
        fdt_path: info.path.into(),
        alias_index: None,
        paddr,
        mapped_base,
        baudrate,
        irq_num: binding_info.irq_num(),
        binding_info,
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
