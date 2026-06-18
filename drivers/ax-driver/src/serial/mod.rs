use alloc::{string::String, vec::Vec};

use ax_errno::AxError;
use fdt_edit::{Fdt, RegFixed};
use log::warn;
use rdrive::{Device, DeviceId, DriverGeneric, register::FdtInfo};
use some_serial::BSerial;
pub use some_serial::{
    BIrqHandler, BRxQueue, BTxQueue, Config, ConfigError, InterruptMask, SerialEvent, SetBackError,
};

mod ns16550;
mod pl011;
mod rockchip_fiq;

use crate::{BindingInfo, binding_info_from_fdt};

struct PlatformSerialDevice {
    name: String,
    info: SerialDeviceInfo,
    interface: Option<BSerial>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialDeviceInfo {
    pub fdt_path: String,
    pub alias_index: Option<usize>,
    pub paddr: usize,
    pub mapped_base: usize,
    pub baudrate: u32,
    pub irq_num: Option<usize>,
    pub rx_polling_required: bool,
    pub binding_info: BindingInfo,
}

pub struct SerialDevice {
    name: String,
    rdrive_device_id: DeviceId,
    info: SerialDeviceInfo,
    interface: BSerial,
}

pub struct SerialRuntimePort {
    name: String,
    rdrive_device_id: DeviceId,
    info: SerialDeviceInfo,
    pub control: SerialRuntimePortControl,
    pub tx: BTxQueue,
    pub rx: BRxQueue,
    pub irq_handler: Option<BIrqHandler>,
}

pub struct SerialRuntimePortControl {
    interface: BSerial,
}

impl PlatformSerialDevice {
    fn new(name: String, info: SerialDeviceInfo, interface: BSerial) -> Self {
        Self {
            name,
            info,
            interface: Some(interface),
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
        self.interface.baudrate()
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num
    }

    pub fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.interface.set_config(config)
    }

    pub fn set_baudrate(&mut self, baudrate: u32) -> Result<(), ConfigError> {
        self.interface.set_config(&Config::new().baudrate(baudrate))
    }

    pub fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.interface.set_irq_mask(mask);
    }

    pub fn get_irq_mask(&self) -> InterruptMask {
        self.interface.get_irq_mask()
    }

    pub fn enable_rx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() | InterruptMask::RX_AVAILABLE;
        self.interface.set_irq_mask(mask);
    }

    pub fn disable_rx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() & !InterruptMask::RX_AVAILABLE;
        self.interface.set_irq_mask(mask);
    }

    pub fn enable_tx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() | InterruptMask::TX_EMPTY;
        self.interface.set_irq_mask(mask);
    }

    pub fn disable_tx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() & !InterruptMask::TX_EMPTY;
        self.interface.set_irq_mask(mask);
    }

    pub fn take_tx(&mut self) -> Option<BTxQueue> {
        self.interface.take_tx()
    }

    pub fn take_rx(&mut self) -> Option<BRxQueue> {
        self.interface.take_rx()
    }

    pub fn take_irq_handler(&mut self) -> Option<BIrqHandler> {
        self.interface.take_irq_handler()
    }

    pub fn set_tx(&mut self, tx: BTxQueue) -> Result<(), SetBackError> {
        self.interface.set_tx(tx)
    }

    pub fn set_rx(&mut self, rx: BRxQueue) -> Result<(), SetBackError> {
        self.interface.set_rx(rx)
    }

    pub fn set_irq_handler(&mut self, irq: BIrqHandler) -> Result<(), SetBackError> {
        self.interface.set_irq_handler(irq)
    }

    pub fn into_runtime_port(mut self) -> Result<SerialRuntimePort, AxError> {
        let tx = self.interface.take_tx().ok_or(AxError::BadState)?;
        let rx = self.interface.take_rx().ok_or(AxError::BadState)?;
        let irq_handler = self.interface.take_irq_handler();
        Ok(SerialRuntimePort {
            name: self.name,
            rdrive_device_id: self.rdrive_device_id,
            info: self.info,
            control: SerialRuntimePortControl {
                interface: self.interface,
            },
            tx,
            rx,
            irq_handler,
        })
    }
}

impl SerialRuntimePort {
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

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num
    }

    pub fn split(
        self,
    ) -> (
        SerialRuntimePortControl,
        BTxQueue,
        BRxQueue,
        Option<BIrqHandler>,
    ) {
        (self.control, self.tx, self.rx, self.irq_handler)
    }
}

impl SerialRuntimePortControl {
    pub fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.interface.set_config(config)
    }

    pub fn set_baudrate(&mut self, baudrate: u32) -> Result<(), ConfigError> {
        self.interface.set_config(&Config::new().baudrate(baudrate))
    }

    pub fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.interface.set_irq_mask(mask);
    }

    pub fn get_irq_mask(&self) -> InterruptMask {
        self.interface.get_irq_mask()
    }

    pub fn enable_rx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() | InterruptMask::RX_AVAILABLE;
        self.interface.set_irq_mask(mask);
    }

    pub fn disable_rx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() & !InterruptMask::RX_AVAILABLE;
        self.interface.set_irq_mask(mask);
    }

    pub fn enable_tx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() | InterruptMask::TX_EMPTY;
        self.interface.set_irq_mask(mask);
    }

    pub fn disable_tx_interrupts(&mut self) {
        let mask = self.interface.get_irq_mask() & !InterruptMask::TX_EMPTY;
        self.interface.set_irq_mask(mask);
    }
}

impl TryFrom<Device<PlatformSerialDevice>> for SerialDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformSerialDevice>) -> Result<Self, Self::Error> {
        let rdrive_device_id = base.descriptor().device_id();
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let info = dev.info.clone();
        let interface = dev.interface.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            rdrive_device_id,
            info,
            interface,
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
        rx_polling_required: serial_rx_polling_required(info.node.as_node()),
        binding_info,
    }
}

fn serial_rx_polling_required(node: &fdt_edit::Node) -> bool {
    node.compatibles().any(|compatible| {
        compatible == "snps,dw-apb-uart" && rdrive::with_fdt(is_cvitek_cv181x).unwrap_or(false)
    })
}

fn is_cvitek_cv181x(fdt: &Fdt) -> bool {
    fdt.node(fdt.root_id()).is_some_and(|root| {
        root.compatibles()
            .any(|compatible| compatible == "cvitek,cv181x")
    })
}

fn serial_binding_info(info: &FdtInfo<'_>, fdt_path: &str) -> BindingInfo {
    binding_info_from_fdt(info).unwrap_or_else(|err| {
        warn!("failed to resolve serial IRQ for {fdt_path}: {err:?}");
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

    #[test]
    fn detects_cvitek_cv181x_root_for_sg2002_serial_polling() {
        let fdt = minimal_serial_alias_fdt_with_root_compatible(&["cvitek,cv181x"]);

        assert!(is_cvitek_cv181x(&fdt));

        let fdt = minimal_serial_alias_fdt_with_root_compatible(&["test,board"]);

        assert!(!is_cvitek_cv181x(&fdt));
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
