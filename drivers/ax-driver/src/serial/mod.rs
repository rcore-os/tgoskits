use alloc::{format, string::String, vec::Vec};

use ax_errno::AxError;
use fdt_edit::{Fdt, NodeType, RegFixed, Status};
use log::{info, warn};
use rdrive::{
    Device, DriverGeneric,
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
pub use some_serial::{
    BIrqHandler, BRxQueue, BTxQueue, Config, ConfigError, InterruptMask, SerialEvent, SetBackError,
};
use some_serial::{
    BSerial, ns16550,
    ns16550::rockchip_fiq::{ROCKCHIP_FIQ_RK3588_UART_CLOCK, RockchipFiqConfig, RockchipFiqSerial},
    pl011,
};

use crate::{BindingInfo, binding_info_from_fdt};

crate::model_register!(
    name: "common serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl011", "snps,dw-apb-uart", "ns16550a", "ns16550"],
        on_probe: probe
    }],
);

crate::model_register!(
    name: "rockchip fiq debugger serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["rockchip,fiq-debugger"],
        on_probe: probe_rockchip_fiq
    }],
);

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
    info: SerialDeviceInfo,
    interface: BSerial,
}

pub struct SerialRuntimePort {
    name: String,
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
}

impl TryFrom<Device<PlatformSerialDevice>> for SerialDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformSerialDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let info = dev.info.clone();
        let interface = dev.interface.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
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

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();

    info!("Probing serial device: {}", info.node.name());
    let base_reg =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = crate::mmio::iomap(base_reg.address as usize, mmio_size as usize)?;

    let node = info.node.as_node();
    let reg_width = prop_u32(node, "reg-io-width").unwrap_or(1) as usize;
    let reg_shift = prop_u32(node, "reg-shift").map(|shift| 1usize << shift);
    let ns16550_width = reg_shift.unwrap_or(reg_width);
    let mut serial: Option<BSerial> = None;
    for compatible in node.compatibles() {
        if compatible == "arm,pl011" {
            let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(24_000_000);
            serial = Some(pl011::Pl011::new_boxed(mmio_base, clock_freq));
            break;
        }

        if compatible == "snps,dw-apb-uart" {
            let clock_freq =
                prop_u32(node, "clock-frequency").unwrap_or(ns16550::dw_apb::SG2002_UART_CLOCK);
            serial = Some(ns16550::DwApbUart::new_boxed(mmio_base, clock_freq));
            break;
        }

        if matches!(compatible, "ns16550a" | "ns16550") {
            let clock_freq = prop_u32(node, "clock-frequency").unwrap_or(24_000_000);
            serial = Some(ns16550::Ns16550::new_mmio_boxed(
                mmio_base,
                clock_freq,
                ns16550_width,
            ));
            break;
        }
    }

    if let Some(serial) = serial {
        let base = serial.base_addr();
        let baudrate = serial.baudrate();
        let device_info = serial_device_info(&info, &base_reg, base, baudrate);
        info!("Serial@{base:#x} registered successfully");
        plat_dev.register(PlatformSerialDevice::new(
            serial.name().into(),
            device_info,
            serial,
        ));
    }

    Ok(())
}

fn probe_rockchip_fiq(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let live_fdt =
        rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))?;
    let fdt_config = rockchip_fiq_fdt_config(&live_fdt, info.node)?;
    let mmio_base = crate::mmio::iomap(
        fdt_config.reg.address as usize,
        fdt_config.reg.size.unwrap_or(0x100) as usize,
    )?;

    if fdt_config.target_disabled {
        info!(
            "Rockchip FIQ debugger takes disabled UART alias serial{} at {}",
            fdt_config.config.serial_id, fdt_config.uart_path
        );
    }

    let serial = RockchipFiqSerial::new_boxed(mmio_base, fdt_config.config);
    let base = serial.base_addr();
    info!(
        "Rockchip FIQ debugger UART@{base:#x} registered successfully, serial-id={}, baudrate={}, \
         irq-mode={}",
        fdt_config.config.serial_id, fdt_config.config.baudrate, fdt_config.config.irq_mode_enabled
    );
    let binding_info = if fdt_config.config.irq_mode_enabled {
        serial_binding_info(&info, &fdt_config.uart_path)
    } else {
        BindingInfo::empty()
    };
    plat_dev.register(PlatformSerialDevice::new(
        serial.name().into(),
        SerialDeviceInfo {
            fdt_path: fdt_config.uart_path,
            alias_index: Some(fdt_config.config.serial_id as usize),
            paddr: fdt_config.reg.address as usize,
            mapped_base: base,
            baudrate: serial.baudrate(),
            irq_num: binding_info.irq_num(),
            rx_polling_required: true,
            binding_info,
        },
        serial,
    ));
    Ok(())
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

struct RockchipFiqFdtConfig {
    config: RockchipFiqConfig,
    reg: RegFixed,
    uart_path: String,
    target_disabled: bool,
}

fn rockchip_fiq_fdt_config(
    fdt: &Fdt,
    fiq: NodeType<'_>,
) -> Result<RockchipFiqFdtConfig, OnProbeError> {
    let fiq_node = fiq.as_node();
    let serial_id = prop_u32(fiq_node, "rockchip,serial-id").ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no rockchip,serial-id", fiq.name()))
    })?;

    if serial_id == u32::MAX {
        return Err(OnProbeError::NotMatch);
    }

    let alias = format!("serial{serial_id}");
    let uart_path = fdt
        .resolve_alias(&alias)
        .map(String::from)
        .ok_or_else(|| OnProbeError::other(format!("{alias} alias not found")))?;
    let uart_node = fdt
        .get_by_path(&uart_path)
        .ok_or_else(|| OnProbeError::other(format!("{uart_path} node not found")))?;
    let uart = uart_node.as_node();

    if !uart
        .compatibles()
        .any(|compatible| compatible == "snps,dw-apb-uart")
    {
        return Err(OnProbeError::other(format!(
            "{uart_path} is not a snps,dw-apb-uart node"
        )));
    }

    let reg_width = prop_u32(uart, "reg-io-width").unwrap_or(4);
    let reg_shift = prop_u32(uart, "reg-shift").unwrap_or(2);
    if reg_width != 4 || reg_shift != 2 {
        return Err(OnProbeError::other(format!(
            "{uart_path} has unsupported reg-io-width/reg-shift {reg_width}/{reg_shift}"
        )));
    }

    let reg = uart_node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{uart_path}] has no reg")))?;

    let baudrate = normalise_fiq_baudrate(
        prop_u32(fiq_node, "rockchip,baudrate")
            .unwrap_or(some_serial::ns16550::rockchip_fiq::ROCKCHIP_FIQ_DEFAULT_BAUDRATE),
    );
    let clock_hz = prop_u32(uart, "clock-frequency").unwrap_or(ROCKCHIP_FIQ_RK3588_UART_CLOCK);
    let irq_mode_enabled = prop_u32(fiq_node, "rockchip,irq-mode-enable").unwrap_or(0) != 0;
    let target_disabled = matches!(uart.status(), Some(Status::Disabled));

    if matches!(uart.status(), Some(status) if status != Status::Disabled && status != Status::Okay)
    {
        warn!("{uart_path} has unrecognised status; proceeding for FIQ debugger");
    }

    Ok(RockchipFiqFdtConfig {
        config: RockchipFiqConfig {
            serial_id,
            baudrate,
            clock_hz,
            irq_mode_enabled,
            debug_enable: true,
            console_enable: true,
        },
        reg,
        uart_path,
        target_disabled,
    })
}

fn normalise_fiq_baudrate(baudrate: u32) -> u32 {
    match baudrate {
        115_200 | 1_500_000 => baudrate,
        other => {
            warn!("unsupported rockchip fiq baudrate {other}, falling back to 115200");
            115_200
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn resolves_fiq_debugger_target_uart_from_alias_even_when_uart_disabled() {
        let fdt = minimal_fiq_fdt(true, true);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");

        let config = rockchip_fiq_fdt_config(&fdt, fiq).expect("parse fiq config");

        assert_eq!(config.config.serial_id, 2);
        assert_eq!(config.config.baudrate, 1_500_000);
        assert_eq!(config.config.clock_hz, ROCKCHIP_FIQ_RK3588_UART_CLOCK);
        assert!(config.config.irq_mode_enabled);
        assert_eq!(config.uart_path, "/serial@feb50000");
        assert!(config.target_disabled);
        assert_eq!(config.reg.address, 0xfeb5_0000);
        assert_eq!(config.reg.size, Some(0x100));
    }

    #[test]
    fn rejects_missing_alias_or_non_dw_apb_target() {
        let fdt = minimal_fiq_fdt(false, true);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");
        assert!(rockchip_fiq_fdt_config(&fdt, fiq).is_err());

        let fdt = minimal_fiq_fdt(true, false);
        let fiq = fdt.get_by_path("/fiq-debugger").expect("fiq node missing");
        assert!(rockchip_fiq_fdt_config(&fdt, fiq).is_err());
    }

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

    fn minimal_fiq_fdt(with_alias: bool, dw_apb: bool) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32_ls("#address-cells", &[2]));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32_ls("#size-cells", &[1]));

        let aliases = fdt.add_node(root, Node::new("aliases"));
        if with_alias {
            fdt.node_mut(aliases)
                .unwrap()
                .set_property(prop_str("serial2", "/serial@feb50000"));
        }

        let fiq = fdt.add_node(root, Node::new("fiq-debugger"));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_strs("compatible", &["rockchip,fiq-debugger"]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,serial-id", &[2]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,baudrate", &[1_500_000]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_u32_ls("rockchip,irq-mode-enable", &[1]));
        fdt.node_mut(fiq)
            .unwrap()
            .set_property(prop_str("status", "okay"));

        let uart = fdt.add_node(root, Node::new("serial@feb50000"));
        fdt.node_mut(uart).unwrap().set_property(prop_strs(
            "compatible",
            if dw_apb {
                &["rockchip,rk3588-uart", "snps,dw-apb-uart"]
            } else {
                &["rockchip,rk3588-uart"]
            },
        ));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_reg(0xfeb5_0000, 0x100));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_u32_ls("reg-io-width", &[4]));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_u32_ls("reg-shift", &[2]));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(prop_str("status", "disabled"));
        fdt
    }

    fn prop_u32_ls(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        Property::new(name, data)
    }

    fn prop_reg(address: u64, size: u32) -> Property {
        let mut data = Vec::new();
        data.extend_from_slice(&((address >> 32) as u32).to_be_bytes());
        data.extend_from_slice(&(address as u32).to_be_bytes());
        data.extend_from_slice(&size.to_be_bytes());
        Property::new("reg", data)
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
