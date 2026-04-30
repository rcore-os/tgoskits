#![no_std]

use core::{any::Any, mem::size_of};

use log::{info, warn};
use mmio_api::MmioRaw;
use rdif_pcie::{DriverGeneric, Interface, PciAddress};
use thiserror::Error;

const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_REVISION_CLASS_OFFSET: usize = 0x08;
const PCI_PRIMARY_BUS_OFFSET: usize = 0x18;
const PCI_COMMAND_IO: u32 = 1 << 0;
const PCI_COMMAND_MEMORY: u32 = 1 << 1;
const PCI_COMMAND_MASTER: u32 = 1 << 2;
const PCI_COMMAND_SERR: u32 = 1 << 8;
const PCI_CLASS_BRIDGE_PCI: u32 = (0x06 << 24) | (0x04 << 16);

const PCIE_CLIENT_GENERAL_CTRL: usize = 0x000;
const PCIE_CLIENT_INTR_MASK: usize = 0x024;
const PCIE_CLIENT_POWER: usize = 0x02c;
const PCIE_CLIENT_GENERAL_DEBUG: usize = 0x104;
const PCIE_CLIENT_HOT_RESET_CTRL: usize = 0x180;
const PCIE_CLIENT_LTSSM_STATUS: usize = 0x300;
const PCIE_LTSSM_APP_DLY2_EN: u32 = 1 << 1;
const PCIE_LTSSM_ENABLE_ENHANCE: u32 = 1 << 4;
const PCIE_CLIENT_RESET_MASK: u32 = 1 << 18;

const PCIE_PORT_DEBUG1: usize = 0x72c;
const PCIE_PORT_DEBUG1_LINK_UP: u32 = 1 << 4;
const PCIE_LINK_WIDTH_SPEED_CONTROL: usize = 0x80c;
const PORT_LOGIC_SPEED_CHANGE: u32 = 1 << 17;
const PCIE_MISC_CONTROL_1_OFF: usize = 0x8bc;
const PCIE_DBI_RO_WR_EN: u32 = 1 << 0;

const DEFAULT_DBI_ATU_OFFSET: usize = 0x300000;
const PCIE_ATU_VIEWPORT: usize = 0x900;
const PCIE_ATU_VIEWPORT_CTRL1: usize = 0x904;
const PCIE_ATU_VIEWPORT_CTRL2: usize = 0x908;
const PCIE_ATU_VIEWPORT_LOWER_BASE: usize = 0x90c;
const PCIE_ATU_VIEWPORT_UPPER_BASE: usize = 0x910;
const PCIE_ATU_VIEWPORT_LIMIT: usize = 0x914;
const PCIE_ATU_VIEWPORT_LOWER_TARGET: usize = 0x918;
const PCIE_ATU_VIEWPORT_UPPER_TARGET: usize = 0x91c;
const PCIE_ATU_VIEWPORT_UPPER_LIMIT: usize = 0x924;
const PCIE_ATU_UNROLL_REGION_SIZE: usize = 0x200;
const PCIE_ATU_UNROLL_REGION_INDEX_MASK: u32 = 0x1f;
const PCIE_ATU_CTRL1: usize = 0x00;
const PCIE_ATU_CTRL2: usize = 0x04;
const PCIE_ATU_LOWER_BASE: usize = 0x08;
const PCIE_ATU_UPPER_BASE: usize = 0x0c;
const PCIE_ATU_LIMIT: usize = 0x10;
const PCIE_ATU_LOWER_TARGET: usize = 0x14;
const PCIE_ATU_UPPER_TARGET: usize = 0x18;
const PCIE_ATU_UPPER_LIMIT: usize = 0x20;
const PCIE_ATU_ENABLE: u32 = 1 << 31;
const PCIE_ATU_TYPE_MEM: u32 = 0x0;
const PCIE_ATU_TYPE_CFG0: u32 = 0x4;
const PCIE_ATU_TYPE_CFG1: u32 = 0x5;

const PCIE_LINK_WAIT_US: u64 = 10_000;
const PCIE_LINK_WAIT_RETRIES: usize = 80;
const PCIE_LINK_STABLE_WAIT_MS: u64 = 50;
const RK3588_PCIE_PERST_INACTIVE_MS: u64 = 200;
const CFG_ATU_REGION: u8 = 0;

pub const MEM_ATU_FIRST_REGION: u8 = 1;

pub trait Delay {
    fn delay_us(&self, us: u64);
    fn delay_ms(&self, ms: u64);
}

pub trait ResetControl {
    fn assert_perst(&mut self);
    fn deassert_perst(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IatuMode {
    Unroll,
    Viewport,
}

impl IatuMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unroll => "unroll",
            Self::Viewport => "viewport",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostConfig {
    pub apb_phys: u64,
    pub cfg_phys: u64,
    pub cfg_size: usize,
    pub bus_base: u8,
    pub logical_bus_end: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkReport {
    pub link_up: bool,
    pub firmware_trained: bool,
    pub ltssm_status: u32,
    pub debug1: u32,
    pub iatu_mode: IatuMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundWindow {
    pub cpu_base: u64,
    pub pci_base: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointInfo {
    pub address: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision_id: u8,
    pub base_class: u8,
    pub sub_class: u8,
    pub prog_if: u8,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum Error {
    #[error("invalid outbound iATU window cpu={cpu_base:#x} size={size:#x}")]
    InvalidOutboundWindow { cpu_base: u64, size: u64 },
}

pub struct Rk3588PcieHost {
    apb: MmioRaw,
    dbi: MmioRaw,
    cfg: MmioRaw,
    apb_phys: u64,
    cfg_phys: u64,
    cfg_size: usize,
    bus_base: u8,
    logical_bus_end: u8,
    cfg_bus_delta: i16,
    iatu_mode: IatuMode,
}

impl Rk3588PcieHost {
    pub fn new(
        apb: MmioRaw,
        dbi: MmioRaw,
        cfg: MmioRaw,
        config: HostConfig,
    ) -> Result<Self, Error> {
        let iatu_mode = if read32(&dbi, PCIE_ATU_VIEWPORT) == u32::MAX {
            IatuMode::Unroll
        } else {
            IatuMode::Viewport
        };

        Ok(Self {
            apb,
            dbi,
            cfg,
            apb_phys: config.apb_phys,
            cfg_phys: config.cfg_phys,
            cfg_size: config.cfg_size,
            bus_base: config.bus_base,
            logical_bus_end: config.logical_bus_end,
            cfg_bus_delta: i16::from(config.bus_base),
            iatu_mode,
        })
    }

    pub fn init(
        &mut self,
        delay: &dyn Delay,
        mut reset: Option<&mut dyn ResetControl>,
    ) -> LinkReport {
        self.enable_dbi_ro_writes();
        self.force_root_complex_mode();
        self.program_root_bridge_defaults();

        let firmware_trained = self.link_up();
        if firmware_trained {
            let report = self.link_report(true);
            info!(
                "Rockchip RK3588 PCIe host {:#x}: preserving firmware-trained PCIe link, \
                 LTSSM={:#x}, iATU={}",
                self.apb_phys,
                report.ltssm_status,
                report.iatu_mode.as_str()
            );
            self.detect_cfg_bus_delta();
            return report;
        }

        if let Some(reset) = reset.as_mut() {
            reset.assert_perst();
        }

        write32(&self.apb, PCIE_CLIENT_GENERAL_CTRL, 0x000c_0008);
        write32(&self.apb, PCIE_CLIENT_GENERAL_DEBUG, 0);
        write32(&self.apb, PCIE_CLIENT_INTR_MASK, PCIE_CLIENT_RESET_MASK);

        update32(&self.apb, PCIE_CLIENT_HOT_RESET_CTRL, |value| {
            let bits = PCIE_LTSSM_ENABLE_ENHANCE | PCIE_LTSSM_APP_DLY2_EN;
            value | bits | (bits << 16)
        });

        write32(&self.apb, PCIE_CLIENT_GENERAL_CTRL, 0x000c_000c);
        if let Some(reset) = reset.as_mut() {
            delay.delay_ms(RK3588_PCIE_PERST_INACTIVE_MS);
            reset.deassert_perst();
            delay.delay_ms(1);
        }
        update32(&self.dbi, PCIE_LINK_WIDTH_SPEED_CONTROL, |value| {
            value | PORT_LOGIC_SPEED_CHANGE
        });

        if self.wait_link_up(delay) {
            delay.delay_ms(PCIE_LINK_STABLE_WAIT_MS);
            let report = self.link_report(false);
            info!(
                "Rockchip RK3588 PCIe host {:#x}: link up, LTSSM={:#x}, iATU={}",
                self.apb_phys,
                report.ltssm_status,
                report.iatu_mode.as_str()
            );
        } else {
            let report = self.link_report(false);
            warn!(
                "Rockchip RK3588 PCIe host {:#x}: link down, LTSSM={:#x}, DEBUG1={:#x}",
                self.apb_phys, report.ltssm_status, report.debug1
            );
        }

        self.detect_cfg_bus_delta();
        self.link_report(false)
    }

    pub fn apb_phys(&self) -> u64 {
        self.apb_phys
    }

    pub fn iatu_mode(&self) -> IatuMode {
        self.iatu_mode
    }

    pub fn link_up(&self) -> bool {
        read32(&self.dbi, PCIE_PORT_DEBUG1) & PCIE_PORT_DEBUG1_LINK_UP != 0
    }

    pub fn ltssm_status(&self) -> u32 {
        read32(&self.apb, PCIE_CLIENT_LTSSM_STATUS)
    }

    pub fn debug1(&self) -> u32 {
        read32(&self.dbi, PCIE_PORT_DEBUG1)
    }

    pub fn program_memory_window(&self, region: u8, window: OutboundWindow) -> Result<(), Error> {
        self.program_outbound_atu(
            region,
            PCIE_ATU_TYPE_MEM,
            window.cpu_base,
            window.pci_base,
            window.size,
        )
    }

    pub fn direct_endpoint_info(&self) -> Option<EndpointInfo> {
        if !self.link_up() {
            return None;
        }

        let address = PciAddress::new(0, 1, 0, 0);
        let id = self.read_config32(address, 0).unwrap_or(u32::MAX);
        let vendor_id = (id & 0xffff) as u16;
        if vendor_id == 0xffff {
            return None;
        }

        let device_id = (id >> 16) as u16;
        let class = self
            .read_config32(address, PCI_REVISION_CLASS_OFFSET as u16)
            .unwrap_or(0);
        Some(EndpointInfo {
            address,
            vendor_id,
            device_id,
            revision_id: (class & 0xff) as u8,
            prog_if: ((class >> 8) & 0xff) as u8,
            sub_class: ((class >> 16) & 0xff) as u8,
            base_class: ((class >> 24) & 0xff) as u8,
        })
    }

    fn link_report(&self, firmware_trained: bool) -> LinkReport {
        LinkReport {
            link_up: self.link_up(),
            firmware_trained,
            ltssm_status: self.ltssm_status(),
            debug1: self.debug1(),
            iatu_mode: self.iatu_mode,
        }
    }

    fn enable_dbi_ro_writes(&self) {
        update32(&self.dbi, PCIE_MISC_CONTROL_1_OFF, |value| {
            value | PCIE_DBI_RO_WR_EN
        });
    }

    fn force_root_complex_mode(&self) {
        write32(&self.apb, PCIE_CLIENT_POWER, 0x3001_1000);
        write32(&self.apb, PCIE_CLIENT_GENERAL_CTRL, 0x00f0_0040);
    }

    fn program_root_bridge_defaults(&self) {
        let revision = read32(&self.dbi, PCI_REVISION_CLASS_OFFSET) & 0xff;
        write32(
            &self.dbi,
            PCI_REVISION_CLASS_OFFSET,
            PCI_CLASS_BRIDGE_PCI | revision,
        );
        write32(
            &self.dbi,
            PCI_PRIMARY_BUS_OFFSET,
            self.physical_bus_number_reg(0, 1, self.logical_bus_end),
        );
        update32(&self.dbi, PCI_COMMAND_OFFSET, |value| {
            value | PCI_COMMAND_IO | PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER | PCI_COMMAND_SERR
        });
    }

    fn wait_link_up(&self, delay: &dyn Delay) -> bool {
        for _ in 0..PCIE_LINK_WAIT_RETRIES {
            if self.link_up() {
                return true;
            }
            delay.delay_us(PCIE_LINK_WAIT_US);
        }
        self.link_up()
    }

    fn valid_root_access(address: PciAddress) -> bool {
        address.bus() == 0 && address.device() == 0 && address.function() == 0
    }

    fn cfg_window_offset_valid(&self, offset: u16) -> bool {
        usize::from(offset) + size_of::<u32>() <= self.cfg_size
    }

    fn valid_child_access(address: PciAddress) -> bool {
        // DesignWare direct downstream config cycles only have a single slot.
        address.bus() != 1 || address.device() == 0
    }

    fn read_config32(&self, address: PciAddress, offset: u16) -> Option<u32> {
        if Self::valid_root_access(address) {
            if usize::from(offset) == PCI_PRIMARY_BUS_OFFSET {
                return Some((u32::from(self.logical_bus_end) << 16) | (1 << 8));
            }
            return Some(read32(&self.dbi, usize::from(offset)));
        }
        if address.bus() == 0 || address.bus() > self.logical_bus_end {
            return None;
        }
        if !Self::valid_child_access(address) {
            return None;
        }
        if !self.link_up() || !self.cfg_window_offset_valid(offset) {
            return None;
        }

        self.program_cfg_atu(address).ok()?;
        Some(read32(&self.cfg, usize::from(offset)))
    }

    fn write_config32(&self, address: PciAddress, offset: u16, value: u32) {
        if Self::valid_root_access(address) {
            let value = if usize::from(offset) == PCI_PRIMARY_BUS_OFFSET {
                self.translate_bus_number_reg(value)
            } else {
                value
            };
            write32(&self.dbi, usize::from(offset), value);
            return;
        }
        if address.bus() == 0 || address.bus() > self.logical_bus_end {
            return;
        }
        if !Self::valid_child_access(address) {
            return;
        }
        if !self.link_up() || !self.cfg_window_offset_valid(offset) {
            return;
        }

        if self.program_cfg_atu(address).is_ok() {
            write32(&self.cfg, usize::from(offset), value);
        }
    }

    fn program_cfg_atu(&self, address: PciAddress) -> Result<(), Error> {
        let atu_type = if address.bus() == 1 {
            PCIE_ATU_TYPE_CFG0
        } else {
            PCIE_ATU_TYPE_CFG1
        };
        let target = self.cfg_target(address, self.cfg_bus(address.bus()));
        self.program_outbound_atu(
            CFG_ATU_REGION,
            atu_type,
            self.cfg_phys,
            target,
            self.cfg_size as u64,
        )
    }

    fn cfg_target(&self, address: PciAddress, bus: u8) -> u64 {
        (u64::from(bus) << 24)
            | ((u64::from(address.device())) << 19)
            | ((u64::from(address.function())) << 16)
    }

    fn root_bus(&self, logical_bus: u8) -> u8 {
        self.bus_base.saturating_add(logical_bus)
    }

    fn cfg_bus(&self, logical_bus: u8) -> u8 {
        let bus = i16::from(logical_bus) + self.cfg_bus_delta;
        bus.clamp(0, i16::from(u8::MAX)) as u8
    }

    fn physical_bus_number_reg(&self, primary: u8, secondary: u8, subordinate: u8) -> u32 {
        u32::from(self.root_bus(primary))
            | (u32::from(self.root_bus(secondary)) << 8)
            | (u32::from(self.root_bus(subordinate)) << 16)
    }

    fn translate_bus_number_reg(&self, value: u32) -> u32 {
        let primary = (value & 0xff) as u8;
        let secondary = ((value >> 8) & 0xff) as u8;
        let subordinate = ((value >> 16) & 0xff) as u8;
        (value & 0xff00_0000) | self.physical_bus_number_reg(primary, secondary, subordinate)
    }

    fn program_outbound_atu(
        &self,
        region: u8,
        atu_type: u32,
        cpu_base: u64,
        pci_base: u64,
        size: u64,
    ) -> Result<(), Error> {
        let Some(cpu_limit) = cpu_base.checked_add(size.saturating_sub(1)) else {
            return Err(Error::InvalidOutboundWindow { cpu_base, size });
        };

        if self.iatu_mode == IatuMode::Unroll {
            let base = DEFAULT_DBI_ATU_OFFSET + usize::from(region) * PCIE_ATU_UNROLL_REGION_SIZE;
            write32(&self.dbi, base + PCIE_ATU_LOWER_BASE, cpu_base as u32);
            write32(
                &self.dbi,
                base + PCIE_ATU_UPPER_BASE,
                (cpu_base >> 32) as u32,
            );
            write32(&self.dbi, base + PCIE_ATU_LIMIT, cpu_limit as u32);
            write32(
                &self.dbi,
                base + PCIE_ATU_UPPER_LIMIT,
                (cpu_limit >> 32) as u32,
            );
            write32(&self.dbi, base + PCIE_ATU_LOWER_TARGET, pci_base as u32);
            write32(
                &self.dbi,
                base + PCIE_ATU_UPPER_TARGET,
                (pci_base >> 32) as u32,
            );
            write32(&self.dbi, base + PCIE_ATU_CTRL1, atu_type);
            write32(&self.dbi, base + PCIE_ATU_CTRL2, PCIE_ATU_ENABLE);
            self.wait_iatu_enabled(base + PCIE_ATU_CTRL2, region);
        } else {
            let viewport = u32::from(region) & PCIE_ATU_UNROLL_REGION_INDEX_MASK;
            write32(&self.dbi, PCIE_ATU_VIEWPORT, viewport);
            write32(&self.dbi, PCIE_ATU_VIEWPORT_LOWER_BASE, cpu_base as u32);
            write32(
                &self.dbi,
                PCIE_ATU_VIEWPORT_UPPER_BASE,
                (cpu_base >> 32) as u32,
            );
            write32(&self.dbi, PCIE_ATU_VIEWPORT_LIMIT, cpu_limit as u32);
            write32(
                &self.dbi,
                PCIE_ATU_VIEWPORT_UPPER_LIMIT,
                (cpu_limit >> 32) as u32,
            );
            write32(&self.dbi, PCIE_ATU_VIEWPORT_LOWER_TARGET, pci_base as u32);
            write32(
                &self.dbi,
                PCIE_ATU_VIEWPORT_UPPER_TARGET,
                (pci_base >> 32) as u32,
            );
            write32(&self.dbi, PCIE_ATU_VIEWPORT_CTRL1, atu_type);
            write32(&self.dbi, PCIE_ATU_VIEWPORT_CTRL2, PCIE_ATU_ENABLE);
            self.wait_iatu_enabled(PCIE_ATU_VIEWPORT_CTRL2, region);
        }

        Ok(())
    }

    fn wait_iatu_enabled(&self, ctrl2_offset: usize, region: u8) {
        for _ in 0..5 {
            if read32(&self.dbi, ctrl2_offset) & PCIE_ATU_ENABLE != 0 {
                return;
            }
            core::hint::spin_loop();
        }
        warn!(
            "PCIe host {:#x}: outbound iATU region {} did not report enabled",
            self.apb_phys, region
        );
    }

    fn detect_cfg_bus_delta(&mut self) {
        if !self.link_up() {
            return;
        }

        let address = PciAddress::new(0, 1, 0, 0);
        let candidates = [self.root_bus(1), 1, self.bus_base, 0];
        let mut seen = [0_u8; 4];
        let mut seen_len = 0;

        for bus in candidates {
            if seen[..seen_len].contains(&bus) {
                continue;
            }
            seen[seen_len] = bus;
            seen_len += 1;

            let id = self.read_config32_with_target_bus(address, 0, bus);
            info!(
                "Rockchip RK3588 PCIe host {:#x}: CFG0 probe target bus {:#x} id {:#010x}",
                self.apb_phys, bus, id
            );
            if id & 0xffff != 0xffff {
                self.cfg_bus_delta = i16::from(bus) - i16::from(address.bus());
                info!(
                    "Rockchip RK3588 PCIe host {:#x}: selected config target bus delta {}",
                    self.apb_phys, self.cfg_bus_delta
                );
                return;
            }
        }
    }

    fn read_config32_with_target_bus(&self, address: PciAddress, offset: u16, bus: u8) -> u32 {
        let atu_type = if address.bus() == 1 {
            PCIE_ATU_TYPE_CFG0
        } else {
            PCIE_ATU_TYPE_CFG1
        };
        let target = self.cfg_target(address, bus);
        if self
            .program_outbound_atu(
                CFG_ATU_REGION,
                atu_type,
                self.cfg_phys,
                target,
                self.cfg_size as u64,
            )
            .is_err()
        {
            return u32::MAX;
        }
        read32(&self.cfg, usize::from(offset))
    }
}

impl DriverGeneric for Rk3588PcieHost {
    fn name(&self) -> &str {
        "Rockchip RK3588 DW PCIe"
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl Interface for Rk3588PcieHost {
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        self.read_config32(address, offset).unwrap_or(u32::MAX)
    }

    fn write(&mut self, address: PciAddress, offset: u16, value: u32) {
        self.write_config32(address, offset, value);
    }
}

fn read32(mmio: &MmioRaw, offset: usize) -> u32 {
    debug_assert!(offset + size_of::<u32>() <= mmio.size());
    mmio.read::<u32>(offset)
}

fn write32(mmio: &MmioRaw, offset: usize, value: u32) {
    debug_assert!(offset + size_of::<u32>() <= mmio.size());
    mmio.write::<u32>(offset, value);
}

fn update32(mmio: &MmioRaw, offset: usize, f: impl FnOnce(u32) -> u32) {
    write32(mmio, offset, f(read32(mmio, offset)));
}
