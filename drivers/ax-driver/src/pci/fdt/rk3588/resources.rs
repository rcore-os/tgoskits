extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use fdt_edit::{Node, PciRange, Phandle, RegFixed};
use log::{info, warn};
use mmio_api::{MmioAddr, MmioRaw};
use rdif_pcie::PcieController;
use rdrive::{
    probe::{OnProbeError, fdt::NodeType},
    register::{FdtInfo, ProbeFdt},
};
use rk3588_pci::{Delay, HostConfig, IatuMode, ResetControl, Rk3588PcieHost};

use super::{
    clocks_reset_gpio::{
        assert_resets, clock_specs, deassert_resets, enable_clocks, parse_reset_gpio, parse_resets,
    },
    phy::{init_phys, parse_phys},
    register_fdt_legacy_irq,
    windows::{
        align_up_4k, bus_range_info, config_window, is_config_range, live_fdt, log_direct_endpoint,
        log_resource_summary, program_memory_windows, prop_phandle, set_rk3588_bar_range,
    },
};
use crate::soc::{RockchipPinCtrl, rk3588_enable_power_domain};

pub(super) const RK3588_GPIO_BASES: [u64; 5] = [
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
pub(super) const DEFAULT_CFG_SIZE: u64 = 0x10_0000;
pub(super) const PHY_TYPE_PCIE: u32 = 2;
pub(super) const RK3588_PCIE3PHY_DEFAULT_MODE: u32 = 4;
pub(super) const RK3588_PCIE3PHY_CMN_CON0: usize = 0x000;
pub(super) const RK3588_PCIE3PHY_PHY0_STATUS1: usize = 0x904;
pub(super) const RK3588_PCIE3PHY_PHY1_STATUS1: usize = 0xa04;
pub(super) const PHP_GRF_PCIESEL_CON: usize = 0x100;
pub(super) const PCIE3PHY_SRAM_INIT_DONE: u32 = 1;
pub(super) const BIT_WRITEABLE_SHIFT: u32 = 16;
const RK3588_PCIE_MAX_HOSTS: u32 = 8;
static PROBED_HOST_MASK: AtomicU32 = AtomicU32::new(0);

struct AxDelay;

impl Delay for AxDelay {
    fn delay_us(&self, us: u64) {
        axklib::time::busy_wait(Duration::from_micros(us));
    }

    fn delay_ms(&self, ms: u64) {
        axklib::time::busy_wait(Duration::from_millis(ms));
    }
}

struct Rk3588GpioReset {
    apb_phys: u64,
    bank: u8,
    pin: u8,
    active_high: bool,
    gpio: MmioRaw,
}

impl Rk3588GpioReset {
    fn map(apb_phys: u64, bank: u8, pin: u8, active_high: bool) -> Result<Self, OnProbeError> {
        let phys = *RK3588_GPIO_BASES
            .get(usize::from(bank))
            .ok_or_else(|| OnProbeError::other(format!("invalid RK3588 GPIO bank {}", bank)))?;
        Ok(Self {
            apb_phys,
            bank,
            pin,
            active_high,
            gpio: map_mmio(phys, RK3588_GPIO_SIZE)?,
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
        self.gpio.write::<u32>(offset, mask | data);
    }
}

impl ResetControl for Rk3588GpioReset {
    fn assert_perst(&mut self) {
        self.set_logical(false);
        info!(
            "Rockchip RK3588 PCIe host {:#x}: assert PERST via GPIO{} pin {}",
            self.apb_phys, self.bank, self.pin
        );
    }

    fn deassert_perst(&mut self) {
        self.set_logical(true);
        info!(
            "Rockchip RK3588 PCIe host {:#x}: release PERST after {}ms",
            self.apb_phys, RK3588_PCIE_PERST_INACTIVE_MS
        );
    }
}

pub(super) struct RegMmio {
    mmio: MmioRaw,
    size: usize,
}

impl RegMmio {
    pub(super) fn map_phandle(phandle: Phandle, context: &str) -> Result<Self, OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!("{context} phandle {phandle:?} not found"))
        })?;
        let reg = node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(format!("[{}] has no reg for {context}", node.name()))
        })?;
        Self::map_reg(reg)
    }

    pub(super) fn map_reg(reg: RegFixed) -> Result<Self, OnProbeError> {
        let size = align_up_4k((reg.size.unwrap_or(0x1000) as usize).max(1));
        let mmio = map_mmio(reg.address, size)?;
        Ok(Self { mmio, size })
    }

    pub(super) fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.size);
        self.mmio.read::<u32>(offset)
    }

    pub(super) fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.size);
        self.mmio.write::<u32>(offset, value);
    }

    pub(super) fn update32(&self, offset: usize, mask: u32, value: u32) {
        let current = self.read32(offset);
        self.write32(offset, (current & !mask) | value);
    }
}

#[derive(Clone)]
pub(super) struct ClockSpec {
    pub(super) name: Option<String>,
    pub(super) id: u32,
    pub(super) assigned_rate: Option<u32>,
}

#[derive(Clone)]
pub(super) struct ResetSpec {
    pub(super) name: Option<String>,
    pub(super) id: u64,
}

#[derive(Clone, Copy)]
pub(super) struct GpioSpec {
    pub(super) bank: u8,
    pub(super) pin: u8,
    pub(super) active_high: bool,
}

pub(super) struct HostResources<'a> {
    pub(super) name: String,
    pub(super) node: NodeType<'a>,
    pub(super) apb: RegFixed,
    pub(super) dbi: RegFixed,
    pub(super) cfg_phys: u64,
    pub(super) cfg_size: u64,
    pub(super) ranges: Vec<PciRange>,
    pub(super) bus_base: u8,
    pub(super) logical_bus_end: u8,
    pub(super) power_domains: Vec<usize>,
    pub(super) clocks: Vec<ClockSpec>,
    pub(super) resets: Vec<ResetSpec>,
    pub(super) pipe_grf: Option<Phandle>,
    pub(super) reset_gpio: Option<GpioSpec>,
    pub(super) supply: Option<Phandle>,
    pub(super) phys: Vec<PhyRef>,
}

#[derive(Clone)]
pub(super) struct PhyRef {
    pub(super) phandle: Phandle,
    pub(super) specifier: Vec<u32>,
    pub(super) name: Option<String>,
}

pub(super) struct Pcie3PhyResources {
    pub(super) name: String,
    pub(super) reg: RegFixed,
    pub(super) phy_grf: Phandle,
    pub(super) pipe_grf: Option<Phandle>,
    pub(super) pcie30_phymode: u32,
    pub(super) clocks: Vec<ClockSpec>,
    pub(super) resets: Vec<ResetSpec>,
}

pub(super) struct CombphyResources {
    pub(super) name: String,
    pub(super) reg: RegFixed,
    pub(super) pipe_grf: Phandle,
    pub(super) pipe_phy_grf: Phandle,
    pub(super) pcie1ln_sel_bits: Option<[u32; 4]>,
    pub(super) refclk_rate: u32,
    pub(super) clocks: Vec<ClockSpec>,
    pub(super) resets: Vec<ResetSpec>,
}

pub(super) fn probe_rk3588(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let NodeType::Pci(node) = info.node else {
        return Err(OnProbeError::NotMatch);
    };

    let resources = parse_host_resources(&info, NodeType::Pci(node))?;
    if !claim_host_probe(resources.apb.address) {
        return Err(OnProbeError::NotMatch);
    }
    let mut reset = resources
        .reset_gpio
        .map(|gpio| {
            Rk3588GpioReset::map(resources.apb.address, gpio.bank, gpio.pin, gpio.active_high)
        })
        .transpose()?;
    prepare_controller_resources(&resources)?;

    let apb_size = resources.apb.size.unwrap_or(0x10000) as usize;
    let dbi_size = resources.dbi.size.unwrap_or(0x400000) as usize;
    let apb = map_mmio(resources.apb.address, apb_size)?;
    let dbi = map_mmio(resources.dbi.address, dbi_size)?;
    let cfg = map_mmio(resources.cfg_phys, resources.cfg_size as usize)?;

    let mut host = Rk3588PcieHost::new(
        apb,
        dbi,
        cfg,
        HostConfig {
            apb_phys: resources.apb.address,
            cfg_phys: resources.cfg_phys,
            cfg_size: resources.cfg_size as usize,
            bus_base: resources.bus_base,
            logical_bus_end: resources.logical_bus_end,
            iatu_mode: IatuMode::Unroll,
        },
    );

    let delay = AxDelay;
    match reset.as_mut() {
        Some(reset) => {
            host.init(&delay, Some(reset));
        }
        None => {
            host.init(&delay, None);
        }
    }

    program_memory_windows(
        &host,
        &resources.ranges,
        resources.cfg_phys,
        resources.cfg_size,
    );
    host.unmask_legacy_intx_all();
    info!(
        "Rockchip RK3588 PCIe host {:#x}: legacy INTx unmasked",
        host.apb_phys()
    );
    log_direct_endpoint(&host);
    register_fdt_legacy_irq(&info, resources.logical_bus_end);

    let mut drv = PcieController::new(host);
    for range in &resources.ranges {
        if is_config_range(range, resources.cfg_phys, resources.cfg_size) {
            continue;
        }
        set_rk3588_bar_range(&mut drv, range);
    }

    info!(
        "Rockchip RK3588 PCIe host {:#x}: registering config window {:#x}/{} bytes, DT buses \
         {:#x}..={:#x}, logical buses 0..={}",
        resources.apb.address,
        resources.cfg_phys,
        resources.cfg_size,
        resources.bus_base,
        resources.bus_base.saturating_add(resources.logical_bus_end),
        resources.logical_bus_end
    );
    plat_dev.register_pcie(drv);
    Ok(())
}

fn claim_host_probe(apb_base: u64) -> bool {
    let Some(index) = apb_base
        .checked_sub(0xfe15_0000)
        .filter(|offset| offset % 0x1_0000 == 0)
        .map(|offset| (offset / 0x1_0000) as u32)
        .filter(|index| *index < RK3588_PCIE_MAX_HOSTS)
    else {
        return true;
    };
    let bit = 1_u32 << index;
    PROBED_HOST_MASK.fetch_or(bit, Ordering::AcqRel) & bit == 0
}

fn map_mmio(phys: u64, size: usize) -> Result<MmioRaw, OnProbeError> {
    let virt = crate::mmio::iomap(phys as usize, size)?;
    Ok(unsafe { MmioRaw::new(MmioAddr::from(phys), virt, size) })
}

fn prepare_controller_resources(resources: &HostResources<'_>) -> Result<(), OnProbeError> {
    let delay = AxDelay;
    if let Some(gpio) = resources.reset_gpio {
        let mut reset =
            Rk3588GpioReset::map(resources.apb.address, gpio.bank, gpio.pin, gpio.active_high)?;
        reset.assert_perst();
    } else {
        warn!(
            "Rockchip RK3588 PCIe host {:#x}: no PERST GPIO discovered",
            resources.apb.address
        );
    }

    enable_vpcie3v3_supply(resources.supply)?;
    enable_power_domains(&resources.power_domains)?;
    init_phys(resources.node, &resources.phys)?;
    assert_resets(&resources.resets)?;
    delay.delay_us(1);
    deassert_resets(&resources.resets)?;
    enable_clocks(&resources.clocks)?;
    axklib::time::busy_wait(Duration::from_millis(1));
    log_resource_summary(resources);
    Ok(())
}

fn parse_power_domains(node: &Node) -> Result<Vec<usize>, OnProbeError> {
    let Some(prop) = node.get_property("power-domains") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed power-domains",
            node.name()
        )));
    }
    Ok(cells.chunks(2).map(|chunk| chunk[1] as usize).collect())
}

fn enable_power_domains(domains: &[usize]) -> Result<(), OnProbeError> {
    if domains.is_empty() {
        return Ok(());
    }

    for &domain in domains {
        rk3588_enable_power_domain(domain).map_err(|err| {
            OnProbeError::other(format!(
                "failed to enable RK3588 PCIe power domain {domain}: {err}"
            ))
        })?;
    }
    Ok(())
}

fn enable_vpcie3v3_supply(supply: Option<Phandle>) -> Result<(), OnProbeError> {
    let Some(supply) = supply else {
        return Ok(());
    };
    let pinctrl = rdrive::get_one::<RockchipPinCtrl>()
        .ok_or_else(|| OnProbeError::other("RockchipPinCtrl not found for PCIe regulator"))?;
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock RockchipPinCtrl: {err}")))?;
    pinctrl.enable_fixed_regulator(supply)
}

fn parse_host_resources<'a>(
    info: &FdtInfo<'a>,
    node_type: NodeType<'a>,
) -> Result<HostResources<'a>, OnProbeError> {
    let NodeType::Pci(node) = node_type else {
        return Err(OnProbeError::NotMatch);
    };
    let raw_node = node_type.as_node();
    let node_name = raw_node.name().to_string();
    let regs = node.regs();
    let apb = *regs
        .first()
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no APB register")))?;
    let dbi = *regs
        .get(1)
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no DBI register")))?;
    let ranges = node.ranges().unwrap_or_default();
    let (cfg_phys, cfg_size) = config_window(&regs, &ranges)?;
    let (bus_base, logical_bus_end) = bus_range_info(node.bus_range());

    Ok(HostResources {
        name: node_name,
        node: node_type,
        apb,
        dbi,
        cfg_phys,
        cfg_size,
        ranges,
        bus_base,
        logical_bus_end,
        power_domains: parse_power_domains(raw_node)?,
        clocks: clock_specs(node.clocks()),
        resets: parse_resets(node_type)?,
        pipe_grf: prop_phandle(raw_node, "rockchip,pipe-grf"),
        reset_gpio: parse_reset_gpio(info, apb.address)?,
        supply: prop_phandle(raw_node, "vpcie3v3-supply"),
        phys: parse_phys(node_type)?,
    })
}
