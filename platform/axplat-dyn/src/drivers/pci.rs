extern crate alloc;

use alloc::format;
use core::{
    mem::size_of,
    ptr::{NonNull, read_volatile, write_volatile},
    time::Duration,
};

use fdt_edit::{PciRange, PciSpace, RegFixed};
use heapless::Vec as ArrayVec;
use rdif_pcie::Interface;
use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, fdt::NodeType, pci::*},
    register::FdtInfo,
};
use spin::Mutex;

use super::iomap;

const RK3588_PCIE2L1_APB_BASE: u64 = 0xfe18_0000;
const RK3588_PCIE2L2_APB_BASE: u64 = 0xfe19_0000;

const RK3588_GPIO_BASES: [u64; 5] = [
    0xfd8a_0000,
    0xfec2_0000,
    0xfec3_0000,
    0xfec4_0000,
    0xfec5_0000,
];
const RK3588_GPIO_SIZE: usize = 0x110;
const RK3588_GPIO_SWPORT_DR_L: usize = 0x00;
const RK3588_GPIO_SWPORT_DR_H: usize = 0x04;
const RK3588_GPIO_SWPORT_DDR_L: usize = 0x08;
const RK3588_GPIO_SWPORT_DDR_H: usize = 0x0c;
const RK3588_PCIE_PERST_INACTIVE_MS: u64 = 200;

const DEFAULT_CFG_SIZE: u64 = 0x10_0000;

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
const MAX_PCIE_LEGACY_IRQS: usize = 8;
const CFG_ATU_REGION: u8 = 0;
const MEM_ATU_FIRST_REGION: u8 = 1;

module_driver!(
    name: "Generic PCIe Controller Driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["pci-host-ecam-generic"],
            on_probe: probe_generic_ecam
        }
    ],
);

#[derive(Clone, Copy)]
struct LegacyIrqRoute {
    bus_start: u8,
    bus_end: u8,
    irq: usize,
}

static LEGACY_IRQ_ROUTES: Mutex<ArrayVec<LegacyIrqRoute, MAX_PCIE_LEGACY_IRQS>> =
    Mutex::new(ArrayVec::new());

pub(crate) fn legacy_irq_for_address(address: PciAddress) -> Option<usize> {
    let bus = address.bus();
    LEGACY_IRQ_ROUTES
        .lock()
        .iter()
        .find(|route| bus >= route.bus_start && bus <= route.bus_end)
        .map(|route| route.irq)
}

fn probe_generic_ecam(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let NodeType::Pci(node) = info.node else {
        return Err(OnProbeError::NotMatch);
    };

    let regs = node.regs();
    for reg in &regs {
        trace!(
            "pcie reg: {:#x}, bus: {:#x}",
            reg.address, reg.child_bus_address
        );
    }

    let reg = regs
        .first()
        .ok_or_else(|| OnProbeError::other("PCIe controller has no regs"))?;
    let mmio_base = reg.address as usize;
    let mmio_size = reg.size.unwrap_or(0x1000) as usize;
    let mut drv = new_driver_generic(mmio_base, mmio_size, &crate::boot::Kernel)
        .map_err(|e| OnProbeError::other(format!("failed to create PCIe controller: {e:?}")))?;

    for range in node.ranges().unwrap_or_default() {
        debug!("pcie range {range:?}");
        set_pcie_mem_range(&mut drv, &range);
    }

    plat_dev.register_pcie(drv);

    Ok(())
}

struct MmioRegion {
    base: NonNull<u8>,
    size: usize,
}

unsafe impl Send for MmioRegion {}

impl MmioRegion {
    fn map(phys: u64, size: usize) -> Result<Self, OnProbeError> {
        let base = iomap((phys as usize).into(), size)?;
        Ok(Self { base, size })
    }

    fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + size_of::<u32>() <= self.size);
        unsafe { read_volatile(self.base.as_ptr().add(offset).cast::<u32>()) }
    }

    fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + size_of::<u32>() <= self.size);
        unsafe { write_volatile(self.base.as_ptr().add(offset).cast::<u32>(), value) };
    }

    fn update32(&self, offset: usize, f: impl FnOnce(u32) -> u32) {
        self.write32(offset, f(self.read32(offset)));
    }
}

struct Rk3588GpioReset {
    bank: u8,
    pin: u8,
    active_high: bool,
    gpio: MmioRegion,
}

impl Rk3588GpioReset {
    fn map(bank: u8, pin: u8, active_high: bool) -> Result<Self, OnProbeError> {
        let phys = *RK3588_GPIO_BASES
            .get(usize::from(bank))
            .ok_or_else(|| OnProbeError::other(format!("invalid RK3588 GPIO bank {}", bank)))?;
        Ok(Self {
            bank,
            pin,
            active_high,
            gpio: MmioRegion::map(phys, RK3588_GPIO_SIZE)?,
        })
    }

    fn set_logical(&self, value: bool) {
        let physical = if self.active_high { value } else { !value };
        self.write_masked_pair(RK3588_GPIO_SWPORT_DR_L, RK3588_GPIO_SWPORT_DR_H, physical);
        self.write_masked_pair(RK3588_GPIO_SWPORT_DDR_L, RK3588_GPIO_SWPORT_DDR_H, true);
    }

    fn write_masked_pair(&self, low_offset: usize, high_offset: usize, value: bool) {
        let pin = u32::from(self.pin);
        let (offset, shift) = if pin < 16 {
            (low_offset, pin)
        } else {
            (high_offset, pin - 16)
        };
        let mask = 1_u32 << (shift + 16);
        let data = u32::from(value) << shift;
        self.gpio.write32(offset, mask | data);
    }
}

struct Rk3588DwPcie {
    apb: MmioRegion,
    dbi: MmioRegion,
    cfg: MmioRegion,
    apb_phys: u64,
    cfg_phys: u64,
    cfg_size: usize,
    bus_base: u8,
    logical_bus_end: u8,
    cfg_bus_delta: i16,
    iatu_unroll: bool,
}

impl Rk3588DwPcie {
    fn new(
        apb_reg: RegFixed,
        dbi_reg: RegFixed,
        cfg_phys: u64,
        cfg_size: usize,
        bus_base: u8,
        logical_bus_end: u8,
        reset: Option<Rk3588GpioReset>,
    ) -> Result<Self, OnProbeError> {
        let apb_size = apb_reg.size.unwrap_or(0x10000) as usize;
        let dbi_size = dbi_reg.size.unwrap_or(0x400000) as usize;
        let apb = MmioRegion::map(apb_reg.address, apb_size)?;
        let dbi = MmioRegion::map(dbi_reg.address, dbi_size)?;
        let cfg = MmioRegion::map(cfg_phys, cfg_size)?;
        let iatu_unroll = dbi.read32(PCIE_ATU_VIEWPORT) == u32::MAX;

        let mut host = Self {
            apb,
            dbi,
            cfg,
            apb_phys: apb_reg.address,
            cfg_phys,
            cfg_size,
            bus_base,
            logical_bus_end,
            cfg_bus_delta: i16::from(bus_base),
            iatu_unroll,
        };
        host.init_host(reset.as_ref());
        host.detect_cfg_bus_delta();
        Ok(host)
    }

    fn init_host(&self, reset: Option<&Rk3588GpioReset>) {
        self.enable_dbi_ro_writes();
        self.force_root_complex_mode();
        self.program_root_bridge_defaults();

        if self.link_up() {
            info!(
                "Rockchip RK3588 PCIe host {:#x}: preserving firmware-trained PCIe link, \
                 LTSSM={:#x}, iATU={}",
                self.apb_phys,
                self.apb.read32(PCIE_CLIENT_LTSSM_STATUS),
                if self.iatu_unroll {
                    "unroll"
                } else {
                    "viewport"
                }
            );
            return;
        }

        self.assert_perst(reset);

        self.apb.write32(PCIE_CLIENT_GENERAL_CTRL, 0x000c_0008);
        self.apb.write32(PCIE_CLIENT_GENERAL_DEBUG, 0);
        self.apb
            .write32(PCIE_CLIENT_INTR_MASK, PCIE_CLIENT_RESET_MASK);

        self.apb.update32(PCIE_CLIENT_HOT_RESET_CTRL, |value| {
            let bits = PCIE_LTSSM_ENABLE_ENHANCE | PCIE_LTSSM_APP_DLY2_EN;
            value | bits | (bits << 16)
        });

        self.apb.write32(PCIE_CLIENT_GENERAL_CTRL, 0x000c_000c);
        self.release_perst(reset);
        self.dbi.update32(PCIE_LINK_WIDTH_SPEED_CONTROL, |value| {
            value | PORT_LOGIC_SPEED_CHANGE
        });

        if self.wait_link_up() {
            axklib::time::busy_wait(Duration::from_millis(PCIE_LINK_STABLE_WAIT_MS));
            info!(
                "Rockchip RK3588 PCIe host {:#x}: link up, LTSSM={:#x}, iATU={}",
                self.apb_phys,
                self.apb.read32(PCIE_CLIENT_LTSSM_STATUS),
                if self.iatu_unroll {
                    "unroll"
                } else {
                    "viewport"
                }
            );
        } else {
            warn!(
                "Rockchip RK3588 PCIe host {:#x}: link down, LTSSM={:#x}, DEBUG1={:#x}",
                self.apb_phys,
                self.apb.read32(PCIE_CLIENT_LTSSM_STATUS),
                self.dbi.read32(PCIE_PORT_DEBUG1)
            );
        }
    }

    fn assert_perst(&self, reset: Option<&Rk3588GpioReset>) {
        if let Some(reset) = reset {
            reset.set_logical(true);
            info!(
                "Rockchip RK3588 PCIe host {:#x}: assert PERST via GPIO{} pin {}",
                self.apb_phys, reset.bank, reset.pin
            );
        }
    }

    fn release_perst(&self, reset: Option<&Rk3588GpioReset>) {
        if let Some(reset) = reset {
            axklib::time::busy_wait(Duration::from_millis(RK3588_PCIE_PERST_INACTIVE_MS));
            reset.set_logical(false);
            axklib::time::busy_wait(Duration::from_millis(1));
            info!(
                "Rockchip RK3588 PCIe host {:#x}: release PERST after {}ms",
                self.apb_phys, RK3588_PCIE_PERST_INACTIVE_MS
            );
        }
    }

    fn enable_dbi_ro_writes(&self) {
        self.dbi
            .update32(PCIE_MISC_CONTROL_1_OFF, |value| value | PCIE_DBI_RO_WR_EN);
    }

    fn force_root_complex_mode(&self) {
        self.apb.write32(PCIE_CLIENT_POWER, 0x3001_1000);
        self.apb.write32(PCIE_CLIENT_GENERAL_CTRL, 0x00f0_0040);
    }

    fn program_root_bridge_defaults(&self) {
        let revision = self.dbi.read32(PCI_REVISION_CLASS_OFFSET) & 0xff;
        self.dbi
            .write32(PCI_REVISION_CLASS_OFFSET, PCI_CLASS_BRIDGE_PCI | revision);
        self.dbi.write32(
            PCI_PRIMARY_BUS_OFFSET,
            self.physical_bus_number_reg(0, 1, self.logical_bus_end),
        );
        self.dbi.update32(PCI_COMMAND_OFFSET, |value| {
            value | PCI_COMMAND_IO | PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER | PCI_COMMAND_SERR
        });
    }

    fn wait_link_up(&self) -> bool {
        for _ in 0..PCIE_LINK_WAIT_RETRIES {
            if self.link_up() {
                return true;
            }
            axklib::time::busy_wait(Duration::from_micros(PCIE_LINK_WAIT_US));
        }
        self.link_up()
    }

    fn link_up(&self) -> bool {
        self.dbi.read32(PCIE_PORT_DEBUG1) & PCIE_PORT_DEBUG1_LINK_UP != 0
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
            return Some(self.dbi.read32(usize::from(offset)));
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

        self.program_cfg_atu(address);
        Some(self.cfg.read32(usize::from(offset)))
    }

    fn write_config32(&self, address: PciAddress, offset: u16, value: u32) {
        if Self::valid_root_access(address) {
            let value = if usize::from(offset) == PCI_PRIMARY_BUS_OFFSET {
                self.translate_bus_number_reg(value)
            } else {
                value
            };
            self.dbi.write32(usize::from(offset), value);
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

        self.program_cfg_atu(address);
        self.cfg.write32(usize::from(offset), value);
    }

    fn program_cfg_atu(&self, address: PciAddress) {
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
        );
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
    ) {
        let Some(cpu_limit) = cpu_base.checked_add(size.saturating_sub(1)) else {
            warn!(
                "PCIe host {:#x}: invalid outbound iATU region {} size {:#x}",
                self.apb_phys, region, size
            );
            return;
        };

        if self.iatu_unroll {
            let base = DEFAULT_DBI_ATU_OFFSET + usize::from(region) * PCIE_ATU_UNROLL_REGION_SIZE;
            self.dbi
                .write32(base + PCIE_ATU_LOWER_BASE, cpu_base as u32);
            self.dbi
                .write32(base + PCIE_ATU_UPPER_BASE, (cpu_base >> 32) as u32);
            self.dbi.write32(base + PCIE_ATU_LIMIT, cpu_limit as u32);
            self.dbi
                .write32(base + PCIE_ATU_UPPER_LIMIT, (cpu_limit >> 32) as u32);
            self.dbi
                .write32(base + PCIE_ATU_LOWER_TARGET, pci_base as u32);
            self.dbi
                .write32(base + PCIE_ATU_UPPER_TARGET, (pci_base >> 32) as u32);
            self.dbi.write32(base + PCIE_ATU_CTRL1, atu_type);
            self.dbi.write32(base + PCIE_ATU_CTRL2, PCIE_ATU_ENABLE);
            self.wait_iatu_enabled(base + PCIE_ATU_CTRL2, region);
        } else {
            let viewport = u32::from(region) & PCIE_ATU_UNROLL_REGION_INDEX_MASK;
            self.dbi.write32(PCIE_ATU_VIEWPORT, viewport);
            self.dbi
                .write32(PCIE_ATU_VIEWPORT_LOWER_BASE, cpu_base as u32);
            self.dbi
                .write32(PCIE_ATU_VIEWPORT_UPPER_BASE, (cpu_base >> 32) as u32);
            self.dbi.write32(PCIE_ATU_VIEWPORT_LIMIT, cpu_limit as u32);
            self.dbi
                .write32(PCIE_ATU_VIEWPORT_UPPER_LIMIT, (cpu_limit >> 32) as u32);
            self.dbi
                .write32(PCIE_ATU_VIEWPORT_LOWER_TARGET, pci_base as u32);
            self.dbi
                .write32(PCIE_ATU_VIEWPORT_UPPER_TARGET, (pci_base >> 32) as u32);
            self.dbi.write32(PCIE_ATU_VIEWPORT_CTRL1, atu_type);
            self.dbi.write32(PCIE_ATU_VIEWPORT_CTRL2, PCIE_ATU_ENABLE);
            self.wait_iatu_enabled(PCIE_ATU_VIEWPORT_CTRL2, region);
        }
    }

    fn wait_iatu_enabled(&self, ctrl2_offset: usize, region: u8) {
        for _ in 0..5 {
            if self.dbi.read32(ctrl2_offset) & PCIE_ATU_ENABLE != 0 {
                return;
            }
            axklib::time::busy_wait(Duration::from_micros(10));
        }
        warn!(
            "PCIe host {:#x}: outbound iATU region {} did not report enabled",
            self.apb_phys, region
        );
    }

    fn log_direct_endpoints(&self) {
        if !self.link_up() {
            return;
        }

        let address = PciAddress::new(0, 1, 0, 0);
        let id = self.read_config32(address, 0).unwrap_or(u32::MAX);
        let vendor = (id & 0xffff) as u16;
        if vendor == 0xffff {
            return;
        }

        let device_id = (id >> 16) as u16;
        let class = self
            .read_config32(address, PCI_REVISION_CLASS_OFFSET as u16)
            .unwrap_or(0);
        let revision = (class & 0xff) as u8;
        let prog_if = ((class >> 8) & 0xff) as u8;
        let subclass = ((class >> 16) & 0xff) as u8;
        let base_class = ((class >> 24) & 0xff) as u8;
        info!(
            "PCIe endpoint: {} {:04x}:{:04x} (rev {:02x}, class {:02x}{:02x}{:02x})",
            address, vendor, device_id, revision, base_class, subclass, prog_if
        );
    }

    fn detect_cfg_bus_delta(&mut self) {
        if !self.link_up() {
            return;
        }

        let address = PciAddress::new(0, 1, 0, 0);
        let mut candidates = ArrayVec::<u8, 4>::new();
        let _ = candidates.push(self.root_bus(1));
        let _ = candidates.push(1);
        let _ = candidates.push(self.bus_base);
        let _ = candidates.push(0);

        let mut seen = ArrayVec::<u8, 4>::new();
        for bus in candidates {
            if seen.iter().any(|seen_bus| *seen_bus == bus) {
                continue;
            }
            let _ = seen.push(bus);

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
        self.program_outbound_atu(
            CFG_ATU_REGION,
            atu_type,
            self.cfg_phys,
            target,
            self.cfg_size as u64,
        );
        self.cfg.read32(usize::from(offset))
    }
}

impl DriverGeneric for Rk3588DwPcie {
    fn name(&self) -> &str {
        "Rockchip RK3588 DW PCIe"
    }
}

impl Interface for Rk3588DwPcie {
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        self.read_config32(address, offset).unwrap_or(u32::MAX)
    }

    fn write(&mut self, address: PciAddress, offset: u16, value: u32) {
        self.write_config32(address, offset, value);
    }
}

fn probe_rk3588(
    info: FdtInfo<'_>,
    plat_dev: PlatformDevice,
    expected_apb_base: u64,
) -> Result<(), OnProbeError> {
    let node_name = info.node.as_node().name();
    let NodeType::Pci(node) = info.node else {
        return Err(OnProbeError::NotMatch);
    };

    let regs = node.regs();
    let apb_reg = *regs
        .first()
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no APB register")))?;
    if apb_reg.address != expected_apb_base {
        return Err(OnProbeError::NotMatch);
    }
    let dbi_reg = *regs
        .get(1)
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no DBI register")))?;

    let ranges = node.ranges().unwrap_or_default();
    let (cfg_phys, cfg_size) = config_window(&regs, &ranges)?;
    let (bus_base, logical_bus_end) = bus_range_info(node.bus_range());
    let reset = pcie_reset_gpio(&info, expected_apb_base);

    let host = Rk3588DwPcie::new(
        apb_reg,
        dbi_reg,
        cfg_phys,
        cfg_size as usize,
        bus_base,
        logical_bus_end,
        reset,
    )?;
    program_memory_windows(&host, &ranges, cfg_phys, cfg_size);
    host.log_direct_endpoints();
    register_legacy_irq(&info, logical_bus_end);

    let mut drv = PcieController::new(host);
    for range in &ranges {
        if is_config_range(range, cfg_phys, cfg_size) {
            continue;
        }
        set_rk3588_bar_range(&mut drv, range);
    }

    info!(
        "Rockchip RK3588 PCIe host {:#x}: registering config window {:#x}/{} bytes, DT buses \
         {:#x}..={:#x}, logical buses 0..={}",
        expected_apb_base,
        cfg_phys,
        cfg_size,
        bus_base,
        bus_base.saturating_add(logical_bus_end),
        logical_bus_end
    );
    plat_dev.register_pcie(drv);
    Ok(())
}

fn pcie_reset_gpio(info: &FdtInfo<'_>, apb_base: u64) -> Option<Rk3588GpioReset> {
    let default = rk3588_pcie_reset_pin(apb_base)?;
    let (pin, active_high) = reset_gpio_cells(info).unwrap_or((default.pin, default.active_high));
    if pin != default.pin {
        warn!(
            "Rockchip RK3588 PCIe host {:#x}: reset-gpios pin {} differs from board default \
             GPIO{} pin {}; using board default",
            apb_base, pin, default.bank, default.pin
        );
    }

    match Rk3588GpioReset::map(default.bank, default.pin, active_high) {
        Ok(reset) => Some(reset),
        Err(err) => {
            warn!(
                "Rockchip RK3588 PCIe host {:#x}: failed to map PERST GPIO: {}",
                apb_base, err
            );
            None
        }
    }
}

#[derive(Clone, Copy)]
struct Rk3588ResetPin {
    bank: u8,
    pin: u8,
    active_high: bool,
}

fn rk3588_pcie_reset_pin(apb_base: u64) -> Option<Rk3588ResetPin> {
    match apb_base {
        RK3588_PCIE2L1_APB_BASE => Some(Rk3588ResetPin {
            bank: 3,
            pin: 11,
            active_high: true,
        }),
        RK3588_PCIE2L2_APB_BASE => Some(Rk3588ResetPin {
            bank: 4,
            pin: 2,
            active_high: true,
        }),
        _ => None,
    }
}

fn reset_gpio_cells(info: &FdtInfo<'_>) -> Option<(u8, bool)> {
    let prop = info.node.as_node().get_property("reset-gpios")?;
    let mut cells = prop.get_u32_iter();
    let _phandle = cells.next()?;
    let pin = cells.next()?.try_into().ok()?;
    let flags = cells.next().unwrap_or(0);
    let active_low = flags & 1 != 0;
    Some((pin, !active_low))
}

fn config_window(regs: &[RegFixed], ranges: &[PciRange]) -> Result<(u64, u64), OnProbeError> {
    if let Some(reg) = regs.get(2) {
        return Ok((reg.address, reg.size.unwrap_or(DEFAULT_CFG_SIZE)));
    }

    ranges
        .iter()
        .find(|range| {
            matches!(range.space, PciSpace::Memory32)
                && range.size == DEFAULT_CFG_SIZE
                && range.cpu_address == range.bus_address
        })
        .map(|range| (range.cpu_address, range.size))
        .ok_or_else(|| OnProbeError::other("RK3588 PCIe host has no config window"))
}

fn bus_range_info(bus_range: Option<core::ops::Range<u32>>) -> (u8, u8) {
    let Some(bus_range) = bus_range else {
        return (0, u8::MAX);
    };
    let bus_base = bus_range.start.min(u32::from(u8::MAX)) as u8;
    let logical_end = bus_range
        .end
        .saturating_sub(bus_range.start)
        .clamp(1, u32::from(u8::MAX)) as u8;
    (bus_base, logical_end)
}

fn program_memory_windows(host: &Rk3588DwPcie, ranges: &[PciRange], cfg_phys: u64, cfg_size: u64) {
    let mut region = MEM_ATU_FIRST_REGION;
    for range in ranges {
        if is_config_range(range, cfg_phys, cfg_size) {
            continue;
        }
        match range.space {
            PciSpace::Memory32 | PciSpace::Memory64 => {
                host.program_outbound_atu(
                    region,
                    PCIE_ATU_TYPE_MEM,
                    range.cpu_address,
                    range.bus_address,
                    range.size,
                );
                debug!(
                    "PCIe host {:#x}: iATU mem region {} cpu={:#x} pci={:#x} size={:#x}",
                    host.apb_phys, region, range.cpu_address, range.bus_address, range.size
                );
                region = region.saturating_add(1);
            }
            PciSpace::IO => {}
        }
    }
}

fn is_config_range(range: &PciRange, cfg_phys: u64, cfg_size: u64) -> bool {
    range.cpu_address == cfg_phys && range.size == cfg_size
}

fn set_pcie_mem_range(drv: &mut PcieController, range: &PciRange) {
    match range.space {
        PciSpace::Memory32 => {
            drv.set_mem32(
                PciMem32 {
                    address: range.cpu_address as _,
                    size: range.size as _,
                },
                range.prefetchable,
            );
        }
        PciSpace::Memory64 => {
            drv.set_mem64(
                PciMem64 {
                    address: range.cpu_address,
                    size: range.size,
                },
                range.prefetchable,
            );
        }
        PciSpace::IO => {}
    }
}

fn set_rk3588_bar_range(drv: &mut PcieController, range: &PciRange) {
    set_pcie_mem_range(drv, range);
    if matches!(range.space, PciSpace::Memory32) {
        drv.set_mem64(
            PciMem64 {
                address: range.cpu_address,
                size: range.size,
            },
            range.prefetchable,
        );
    }
}

fn register_legacy_irq(info: &FdtInfo<'_>, logical_bus_end: u8) {
    let Some(interrupt) = info
        .interrupts()
        .into_iter()
        .find(|interrupt| interrupt.name.as_deref() == Some("legacy"))
    else {
        return;
    };
    let Some(parent) = info.phandle_to_device_id(interrupt.interrupt_parent) else {
        warn!(
            "failed to resolve PCIe legacy IRQ parent phandle {}",
            interrupt.interrupt_parent
        );
        return;
    };

    let irq = somehal::irq::irq_setup_by_fdt(parent, &interrupt.specifier).raw();
    let mut routes = LEGACY_IRQ_ROUTES.lock();
    if routes
        .iter()
        .any(|route| route.bus_start == 0 && route.bus_end == logical_bus_end && route.irq == irq)
    {
        return;
    }
    if routes
        .push(LegacyIrqRoute {
            bus_start: 0,
            bus_end: logical_bus_end,
            irq,
        })
        .is_err()
    {
        warn!("too many PCIe legacy IRQ routes; dropping IRQ {}", irq);
    } else {
        info!(
            "PCIe legacy IRQ route: logical bus 0..={} -> IRQ {}",
            logical_bus_end, irq
        );
    }
}

mod rk3588_pcie_fe180000 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host fe180000",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
        probe_rk3588(info, plat_dev, RK3588_PCIE2L1_APB_BASE)
    }
}

mod rk3588_pcie_fe190000 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host fe190000",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[
            ProbeKind::Fdt {
                compatibles: &["rockchip,rk3588-pcie"],
                on_probe: probe
            }
        ],
    );

    fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
        probe_rk3588(info, plat_dev, RK3588_PCIE2L2_APB_BASE)
    }
}

#[cfg(feature = "pci-list-devices")]
mod pci_list_devices {
    use super::*;

    module_driver!(
        name: "PCI Device Lister",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[ProbeKind::Pci {
            on_probe: probe as FnOnProbe
        }],
    );

    fn probe(endpoint: &mut EndpointRc, _plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
        info!("PCIe endpoint: {} bars={:?}", &**endpoint, endpoint.bars());
        Err(OnProbeError::NotMatch)
    }
}
