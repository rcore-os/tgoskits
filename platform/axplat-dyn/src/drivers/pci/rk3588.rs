extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use fdt_edit::{ClockRef, Fdt, Node, PciRange, PciSpace, Phandle, RegFixed};
use mmio_api::{MmioAddr, MmioRaw};
use rdif_pcie::{PciMem64, PcieController};
use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::FdtInfo,
};
use rk3588_pci::{
    Delay, HostConfig, IatuMode, MEM_ATU_FIRST_REGION, OutboundWindow, ResetControl, Rk3588PcieHost,
};

use crate::drivers::soc::{
    RockchipPinCtrl, rk3588_enable_clock, rk3588_enable_power_domain, rk3588_reset_assert,
    rk3588_reset_deassert, rk3588_set_clock_rate,
};

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
const PHY_TYPE_PCIE: u32 = 2;
const RK3588_PCIE3PHY_DEFAULT_MODE: u32 = 4;
const RK3588_PCIE3PHY_CMN_CON0: usize = 0x000;
const RK3588_PCIE3PHY_PHY0_STATUS1: usize = 0x904;
const RK3588_PCIE3PHY_PHY1_STATUS1: usize = 0xa04;
const PHP_GRF_PCIESEL_CON: usize = 0x100;
const PCIE3PHY_SRAM_INIT_DONE: u32 = 1;
const BIT_WRITEABLE_SHIFT: u32 = 16;
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

struct RegMmio {
    mmio: MmioRaw,
    size: usize,
}

impl RegMmio {
    fn map_phandle(phandle: Phandle, context: &str) -> Result<Self, OnProbeError> {
        let fdt = live_fdt()?;
        let node = fdt.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!("{context} phandle {phandle:?} not found"))
        })?;
        let reg = node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(format!("[{}] has no reg for {context}", node.name()))
        })?;
        Self::map_reg(reg)
    }

    fn map_reg(reg: RegFixed) -> Result<Self, OnProbeError> {
        let size = align_up_4k((reg.size.unwrap_or(0x1000) as usize).max(1));
        let mmio = map_mmio(reg.address, size)?;
        Ok(Self { mmio, size })
    }

    fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.size);
        self.mmio.read::<u32>(offset)
    }

    fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + core::mem::size_of::<u32>() <= self.size);
        self.mmio.write::<u32>(offset, value);
    }

    fn update32(&self, offset: usize, mask: u32, value: u32) {
        let current = self.read32(offset);
        self.write32(offset, (current & !mask) | value);
    }
}

#[derive(Clone)]
struct ClockSpec {
    name: Option<String>,
    id: u32,
    assigned_rate: Option<u32>,
}

#[derive(Clone)]
struct ResetSpec {
    name: Option<String>,
    id: u64,
}

#[derive(Clone, Copy)]
struct GpioSpec {
    bank: u8,
    pin: u8,
    active_high: bool,
}

struct HostResources<'a> {
    name: String,
    node: NodeType<'a>,
    apb: RegFixed,
    dbi: RegFixed,
    cfg_phys: u64,
    cfg_size: u64,
    ranges: Vec<PciRange>,
    bus_base: u8,
    logical_bus_end: u8,
    power_domains: Vec<usize>,
    clocks: Vec<ClockSpec>,
    resets: Vec<ResetSpec>,
    pipe_grf: Option<Phandle>,
    reset_gpio: Option<GpioSpec>,
    supply: Option<Phandle>,
    phys: Vec<PhyRef>,
}

#[derive(Clone)]
struct PhyRef {
    phandle: Phandle,
    specifier: Vec<u32>,
    name: Option<String>,
}

struct Pcie3PhyResources {
    name: String,
    reg: RegFixed,
    phy_grf: Phandle,
    pipe_grf: Option<Phandle>,
    pcie30_phymode: u32,
    clocks: Vec<ClockSpec>,
    resets: Vec<ResetSpec>,
}

struct CombphyResources {
    name: String,
    reg: RegFixed,
    pipe_grf: Phandle,
    pipe_phy_grf: Phandle,
    pcie1ln_sel_bits: Option<[u32; 4]>,
    refclk_rate: u32,
    clocks: Vec<ClockSpec>,
    resets: Vec<ResetSpec>,
}

fn probe_rk3588(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
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
    super::register_legacy_irq(&info, resources.logical_bus_end);

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
    let virt = crate::drivers::iomap((phys as usize).into(), size)?;
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

fn clock_specs_for_node(node: NodeType<'_>) -> Vec<ClockSpec> {
    let assigned_clocks = node
        .as_node()
        .get_property("assigned-clocks")
        .map(|prop| {
            let vals = prop.get_u32_iter().collect::<Vec<_>>();
            let mut ids = Vec::new();
            for cells in vals.chunks(2) {
                if let [_, id] = cells {
                    ids.push(*id);
                }
            }
            ids
        })
        .unwrap_or_default();
    let assigned_rates = node
        .as_node()
        .get_property("assigned-clock-rates")
        .map(|prop| prop.get_u32_iter().collect::<Vec<_>>())
        .unwrap_or_default();

    node.clocks()
        .into_iter()
        .filter_map(|clock| {
            let assigned_rate = clock.specifier.first().and_then(|id| {
                assigned_clocks
                    .iter()
                    .position(|assigned| assigned == id)
                    .and_then(|index| assigned_rates.get(index).copied())
                    .filter(|rate| *rate != 0)
            });
            let id = *clock.specifier.first()?;
            Some(ClockSpec {
                name: clock.name,
                id,
                assigned_rate,
            })
        })
        .collect()
}

fn clock_specs(clocks: Vec<ClockRef>) -> Vec<ClockSpec> {
    clocks
        .into_iter()
        .filter_map(|clock| {
            let id = *clock.specifier.first()?;
            Some(ClockSpec {
                name: clock.name,
                id,
                assigned_rate: None,
            })
        })
        .collect()
}

fn enable_clocks(clocks: &[ClockSpec]) -> Result<(), OnProbeError> {
    for clock in clocks {
        let id = clock.id;
        if id == 0 {
            continue;
        }
        if let Some(rate) = clock.assigned_rate {
            rk3588_set_clock_rate(id, u64::from(rate)).map_err(|err| {
                OnProbeError::other(format!(
                    "failed to set RK3588 PCIe clock {:?} ({id:#x}) rate to {rate}: {err}",
                    clock.name
                ))
            })?;
        }
        rk3588_enable_clock(id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to enable RK3588 PCIe clock {:?} ({id:#x}): {err}",
                clock.name
            ))
        })?;
    }
    Ok(())
}

fn parse_resets(node: NodeType<'_>) -> Result<Vec<ResetSpec>, OnProbeError> {
    let Some(prop) = node.as_node().get_property("resets") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed resets",
            node.name()
        )));
    }
    let reset_names = prop_str_list(node.as_node(), "reset-names");
    Ok(cells
        .chunks(2)
        .enumerate()
        .map(|(idx, chunk)| ResetSpec {
            name: reset_names.get(idx).cloned(),
            id: u64::from(chunk[1]),
        })
        .collect())
}

fn assert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_assert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to assert RK3588 PCIe reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    Ok(())
}

fn deassert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_deassert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to deassert RK3588 PCIe reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    Ok(())
}

fn parse_reset_gpio(info: &FdtInfo<'_>, apb_base: u64) -> Result<Option<GpioSpec>, OnProbeError> {
    if let Some(gpio) = parse_gpio_spec(info.node, "reset-gpios")? {
        return Ok(Some(gpio));
    }

    if let Some(default) = rk3588_pcie_reset_pin(apb_base) {
        warn!(
            "Rockchip RK3588 PCIe host {:#x}: reset-gpios missing; using diagnostic fallback \
             GPIO{} pin {}",
            apb_base, default.bank, default.pin
        );
        return Ok(Some(GpioSpec {
            bank: default.bank,
            pin: default.pin,
            active_high: default.active_high,
        }));
    }

    Ok(None)
}

fn parse_gpio_spec(
    node_type: NodeType<'_>,
    prop_name: &str,
) -> Result<Option<GpioSpec>, OnProbeError> {
    let node = node_type.as_node();
    let Some(prop) = node.get_property(prop_name) else {
        return Ok(None);
    };
    let mut cells = prop.get_u32_iter();
    let phandle_raw = cells.next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
    })?;
    let pin = cells.next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has malformed {prop_name}", node.name()))
    })?;
    let flags = cells.next().unwrap_or(0);
    let bank = gpio_bank_from_phandle(Phandle::from(phandle_raw))?;
    Ok(Some(GpioSpec {
        bank,
        pin: pin.try_into().map_err(|_| {
            OnProbeError::other(format!(
                "[{}] {prop_name} pin {pin} does not fit RK3588 GPIO",
                node.name()
            ))
        })?,
        active_high: flags & 1 == 0,
    }))
}

fn gpio_bank_from_phandle(phandle: Phandle) -> Result<u8, OnProbeError> {
    let fdt = live_fdt()?;
    let gpio = fdt
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("GPIO phandle {phandle:?} not found")))?;
    gpio_bank_index(gpio.as_node()).ok_or_else(|| {
        OnProbeError::other(format!(
            "failed to resolve RK3588 GPIO bank for phandle {phandle:?}"
        ))
    })
}

fn gpio_bank_index(node: &Node) -> Option<u8> {
    let name = node.name();
    if let Some(name) = name
        .strip_prefix("gpio")
        .filter(|name| !name.starts_with('@'))
    {
        if let Some(bank) = name
            .chars()
            .next()
            .and_then(|ch| ch.to_digit(10))
            .and_then(|bank| u8::try_from(bank).ok())
            .filter(|bank| usize::from(*bank) < RK3588_GPIO_BASES.len())
        {
            return Some(bank);
        }
    }

    let address = gpio_bank_address(node)?;
    RK3588_GPIO_BASES
        .iter()
        .position(|base| *base == address)
        .and_then(|bank| u8::try_from(bank).ok())
}

fn gpio_bank_address(node: &Node) -> Option<u64> {
    if let Some(address) = node
        .name()
        .split_once('@')
        .and_then(|(_, unit)| u64::from_str_radix(unit, 16).ok())
    {
        return Some(address);
    }

    let reg = node.get_property("reg")?.get_u32_iter().collect::<Vec<_>>();
    match reg.as_slice() {
        [addr] => Some(u64::from(*addr)),
        cells if cells.len() >= 2 => Some((u64::from(cells[0]) << 32) | u64::from(cells[1])),
        _ => None,
    }
}

fn parse_phys(node_type: NodeType<'_>) -> Result<Vec<PhyRef>, OnProbeError> {
    let node = node_type.as_node();
    let Some(prop) = node.get_property("phys") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.is_empty() {
        return Ok(Vec::new());
    }
    let phy_names = prop_str_list(node, "phy-names");
    let mut refs = Vec::new();
    let mut index = 0;
    let mut offset = 0;
    while offset < cells.len() {
        let phandle = Phandle::from(cells[offset]);
        offset += 1;
        let specifier_cells = phy_cells(phandle)?;
        if offset + specifier_cells > cells.len() {
            return Err(OnProbeError::other(format!(
                "[{}] has truncated phys entry for phandle {phandle:?}",
                node.name()
            )));
        }
        let specifier = cells[offset..offset + specifier_cells].to_vec();
        offset += specifier_cells;
        refs.push(PhyRef {
            phandle,
            specifier,
            name: phy_names.get(index).cloned(),
        });
        index += 1;
    }
    Ok(refs)
}

fn init_phys(host_node: NodeType<'_>, phys: &[PhyRef]) -> Result<(), OnProbeError> {
    if phys.is_empty() {
        warn!(
            "Rockchip RK3588 PCIe host {} has no phys property",
            host_node.name()
        );
        return Ok(());
    }
    let fdt = live_fdt()?;
    for phy_ref in phys {
        let phy = fdt.get_by_phandle(phy_ref.phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "PCIe PHY phandle {:?} for {} not found",
                phy_ref.phandle,
                host_node.name()
            ))
        })?;
        if is_compatible(phy.as_node(), "rockchip,rk3588-pcie3-phy") {
            let resources = parse_pcie3_phy(phy)?;
            init_pcie3_phy(&resources)?;
        } else if is_compatible(phy.as_node(), "rockchip,rk3588-naneng-combphy") {
            let Some(&phy_type) = phy_ref.specifier.first() else {
                return Err(OnProbeError::other(format!(
                    "RK3588 combphy {} referenced by {} has no PHY type specifier",
                    phy.name(),
                    host_node.name()
                )));
            };
            if phy_type != PHY_TYPE_PCIE {
                return Err(OnProbeError::other(format!(
                    "RK3588 combphy {} referenced by {} is type {}, expected PCIe",
                    phy.name(),
                    host_node.name(),
                    phy_type
                )));
            }
            let resources = parse_combphy(phy)?;
            init_combphy(&resources)?;
        } else {
            return Err(OnProbeError::other(format!(
                "unsupported RK3588 PCIe PHY {} referenced by {}",
                phy.name(),
                host_node.name()
            )));
        }
    }
    Ok(())
}

fn parse_pcie3_phy(phy: NodeType<'_>) -> Result<Pcie3PhyResources, OnProbeError> {
    let node = phy.as_node();
    let reg = phy
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", phy.name())))?;
    let phy_grf = prop_phandle(node, "rockchip,phy-grf")
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no rockchip,phy-grf", phy.name())))?;
    let mut pcie30_phymode =
        prop_u32(node, "rockchip,pcie30-phymode").unwrap_or(RK3588_PCIE3PHY_DEFAULT_MODE);
    if pcie30_phymode > RK3588_PCIE3PHY_DEFAULT_MODE {
        pcie30_phymode = RK3588_PCIE3PHY_DEFAULT_MODE;
    }
    Ok(Pcie3PhyResources {
        name: phy.name().to_string(),
        reg,
        phy_grf,
        pipe_grf: prop_phandle(node, "rockchip,pipe-grf"),
        pcie30_phymode,
        clocks: clock_specs_for_node(phy),
        resets: parse_resets(phy)?,
    })
}

fn init_pcie3_phy(phy: &Pcie3PhyResources) -> Result<(), OnProbeError> {
    let _mmio = RegMmio::map_reg(phy.reg)?;
    let phy_grf = RegMmio::map_phandle(phy.phy_grf, "rk3588-pcie3-phy rockchip,phy-grf")?;
    let pipe_grf = phy
        .pipe_grf
        .map(|phandle| RegMmio::map_phandle(phandle, "rk3588-pcie3-phy rockchip,pipe-grf"))
        .transpose()?;

    enable_clocks(&phy.clocks)?;
    for reset in &phy.resets {
        rk3588_reset_assert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to assert RK3588 PCIe3 PHY reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    axklib::time::busy_wait(Duration::from_micros(1));

    phy_grf.write32(
        RK3588_PCIE3PHY_CMN_CON0,
        (0x7 << BIT_WRITEABLE_SHIFT) | phy.pcie30_phymode,
    );
    if let Some(pipe_grf) = pipe_grf.as_ref() {
        let mode = phy.pcie30_phymode & 3;
        if mode != 0 {
            pipe_grf.write32(PHP_GRF_PCIESEL_CON, (mode << BIT_WRITEABLE_SHIFT) | mode);
        }
    }
    phy_grf.write32(RK3588_PCIE3PHY_CMN_CON0, (1 << 24) | (1 << 8));

    for reset in &phy.resets {
        rk3588_reset_deassert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to deassert RK3588 PCIe3 PHY reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    poll_pcie3_sram_ready(&phy_grf, RK3588_PCIE3PHY_PHY0_STATUS1, &phy.name)?;
    poll_pcie3_sram_ready(&phy_grf, RK3588_PCIE3PHY_PHY1_STATUS1, &phy.name)?;
    info!(
        "RK3588 PCIe3 PHY {} initialized, mode={}",
        phy.name, phy.pcie30_phymode
    );
    Ok(())
}

fn poll_pcie3_sram_ready(phy_grf: &RegMmio, offset: usize, name: &str) -> Result<(), OnProbeError> {
    for _ in 0..500 {
        if phy_grf.read32(offset) & PCIE3PHY_SRAM_INIT_DONE != 0 {
            return Ok(());
        }
        axklib::time::busy_wait(Duration::from_micros(1));
    }
    Err(OnProbeError::other(format!(
        "RK3588 PCIe3 PHY {name} SRAM ready timeout at GRF offset {offset:#x}"
    )))
}

fn parse_combphy(phy: NodeType<'_>) -> Result<CombphyResources, OnProbeError> {
    let node = phy.as_node();
    let reg = phy
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", phy.name())))?;
    let pipe_grf = prop_phandle(node, "rockchip,pipe-grf")
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no rockchip,pipe-grf", phy.name())))?;
    let pipe_phy_grf = prop_phandle(node, "rockchip,pipe-phy-grf").ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no rockchip,pipe-phy-grf", phy.name()))
    })?;
    let pcie1ln_sel_bits = node
        .get_property("rockchip,pcie1ln-sel-bits")
        .map(|prop| {
            let vals = prop.get_u32_iter().collect::<Vec<_>>();
            if vals.len() != 4 {
                return Err(OnProbeError::other(format!(
                    "[{}] malformed rockchip,pcie1ln-sel-bits",
                    phy.name()
                )));
            }
            Ok([vals[0], vals[1], vals[2], vals[3]])
        })
        .transpose()?;

    Ok(CombphyResources {
        name: phy.name().to_string(),
        reg,
        pipe_grf,
        pipe_phy_grf,
        pcie1ln_sel_bits,
        refclk_rate: assigned_clock_rate(node).unwrap_or(100_000_000),
        clocks: clock_specs_for_node(phy),
        resets: parse_resets(phy)?,
    })
}

fn init_combphy(phy: &CombphyResources) -> Result<(), OnProbeError> {
    let mmio = RegMmio::map_reg(phy.reg)?;
    let pipe_grf = RegMmio::map_phandle(phy.pipe_grf, "rk3588-naneng-combphy rockchip,pipe-grf")?;
    let phy_grf = RegMmio::map_phandle(
        phy.pipe_phy_grf,
        "rk3588-naneng-combphy rockchip,pipe-phy-grf",
    )?;

    assert_resets(&phy.resets)?;
    enable_clocks(&phy.clocks)?;
    if let Some([offset, start, end, value]) = phy.pcie1ln_sel_bits {
        let mask = bit_range_mask(start, end)?;
        pipe_grf.write32(
            offset as usize,
            (mask << BIT_WRITEABLE_SHIFT) | (value << start),
        );
    }
    combphy_update(&mmio, 0x7c, bit_range_mask(4, 5)?, 1 << 4);
    combphy_param_write(&phy_grf, 0x0000, 0, 15, 0x1000)?;
    combphy_param_write(&phy_grf, 0x0004, 0, 15, 0x0000)?;
    combphy_param_write(&phy_grf, 0x0008, 0, 15, 0x0101)?;
    combphy_param_write(&phy_grf, 0x000c, 0, 15, 0x0200)?;

    match phy.refclk_rate {
        24_000_000 => init_combphy_refclk_24m(&mmio, &phy_grf)?,
        25_000_000 => combphy_param_write(&phy_grf, 0x0004, 13, 14, 0x01)?,
        100_000_000 => init_combphy_refclk_100m(&mmio, &phy_grf)?,
        rate => {
            return Err(OnProbeError::other(format!(
                "RK3588 combphy {} unsupported refclk rate {}",
                phy.name, rate
            )));
        }
    }

    combphy_update(&mmio, 0x19 << 2, 1 << 5, 1 << 5);
    deassert_resets(&phy.resets)?;
    info!(
        "RK3588 Naneng combphy {} initialized for PCIe, refclk={}Hz",
        phy.name, phy.refclk_rate
    );
    Ok(())
}

fn init_combphy_refclk_24m(mmio: &RegMmio, phy_grf: &RegMmio) -> Result<(), OnProbeError> {
    combphy_param_write(phy_grf, 0x0004, 13, 14, 0x00)?;
    combphy_update(mmio, 0x20 << 2, bit_range_mask(2, 4)?, 0x4 << 2);
    mmio.write32(0x1b << 2, 0x00);
    mmio.write32(0x0a << 2, 0x90);
    mmio.write32(0x0b << 2, 0x02);
    mmio.write32(0x0d << 2, 0x57);
    mmio.write32(0x0f << 2, 0x5f);
    Ok(())
}

fn init_combphy_refclk_100m(mmio: &RegMmio, phy_grf: &RegMmio) -> Result<(), OnProbeError> {
    combphy_param_write(phy_grf, 0x0004, 13, 14, 0x02)?;
    mmio.write32(0x74, 0xc0);
    combphy_update(mmio, 0x20 << 2, bit_range_mask(2, 4)?, 0x4 << 2);
    mmio.write32(0x1b << 2, 0x4c);
    mmio.write32(0x0a << 2, 0x90);
    mmio.write32(0x0b << 2, 0x43);
    mmio.write32(0x0c << 2, 0x88);
    mmio.write32(0x0d << 2, 0x56);
    Ok(())
}

fn combphy_param_write(
    mmio: &RegMmio,
    offset: usize,
    start: u32,
    end: u32,
    value: u32,
) -> Result<(), OnProbeError> {
    let mask = bit_range_mask(start, end)?;
    mmio.write32(offset, (value << start) | (mask << BIT_WRITEABLE_SHIFT));
    Ok(())
}

fn combphy_update(mmio: &RegMmio, offset: usize, mask: u32, value: u32) {
    mmio.update32(offset, mask, value);
}

fn bit_range_mask(start: u32, end: u32) -> Result<u32, OnProbeError> {
    if start > end || end >= 32 {
        return Err(OnProbeError::other(format!(
            "invalid bit range {}..={}",
            start, end
        )));
    }
    let width = end - start + 1;
    Ok(if width == 32 {
        u32::MAX
    } else {
        ((1_u32 << width) - 1) << start
    })
}

fn assigned_clock_rate(node: &Node) -> Option<u32> {
    node.get_property("assigned-clock-rates")
        .and_then(|prop| prop.get_u32_iter().next())
}

fn log_resource_summary(resources: &HostResources<'_>) {
    info!(
        "Rockchip RK3588 PCIe host {:#x}: FDT resources node={}, dbi={:#x}/{:#x}, \
         cfg={:#x}/{:#x}, buses {:#x}..={:#x}, clocks={}, resets={}, power-domains={}, phys={}, \
         supply={:?}, pipe-grf={:?}, reset-gpio={}",
        resources.apb.address,
        resources.name,
        resources.dbi.address,
        resources.dbi.size.unwrap_or(0),
        resources.cfg_phys,
        resources.cfg_size,
        resources.bus_base,
        resources.bus_base.saturating_add(resources.logical_bus_end),
        resources.clocks.len(),
        resources.resets.len(),
        resources.power_domains.len(),
        resources.phys.len(),
        resources.supply,
        resources.pipe_grf,
        reset_gpio_label(resources.reset_gpio)
    );
    for phy in &resources.phys {
        debug!(
            "Rockchip RK3588 PCIe host {:#x}: PHY {:?} phandle={} specifier={:?}",
            resources.apb.address, phy.name, phy.phandle, phy.specifier
        );
    }
}

fn reset_gpio_label(gpio: Option<GpioSpec>) -> String {
    match gpio {
        Some(gpio) => format!(
            "GPIO{} pin {} active-{}",
            gpio.bank,
            gpio.pin,
            if gpio.active_high { "high" } else { "low" }
        ),
        None => "none".to_string(),
    }
}

fn is_compatible(node: &Node, compatible: &str) -> bool {
    node.compatibles().any(|item| item == compatible)
}

fn phy_cells(phandle: Phandle) -> Result<usize, OnProbeError> {
    let fdt = live_fdt()?;
    let phy = fdt
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("PHY phandle {phandle:?} not found")))?;
    phy.as_node()
        .get_property("#phy-cells")
        .and_then(|prop| prop.get_u32())
        .map(|cells| cells as usize)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] has no #phy-cells for phandle {phandle:?}",
                phy.name()
            ))
        })
}

fn prop_phandle(node: &Node, prop_name: &str) -> Option<Phandle> {
    node.get_property(prop_name)
        .and_then(|prop| prop.get_u32())
        .map(Phandle::from)
}

fn prop_u32(node: &Node, prop_name: &str) -> Option<u32> {
    node.get_property(prop_name).and_then(|prop| prop.get_u32())
}

fn prop_str_list(node: &Node, prop_name: &str) -> Vec<String> {
    node.get_property(prop_name)
        .map(|prop| prop.as_str_iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

fn live_fdt() -> Result<Fdt, OnProbeError> {
    let ptr = somehal::fdt_addr().ok_or_else(|| OnProbeError::other("live FDT not found"))?;
    let ptr = NonNull::new(ptr).ok_or_else(|| OnProbeError::other("live FDT pointer is null"))?;
    unsafe { Fdt::from_ptr(ptr.as_ptr()) }
        .map_err(|err| OnProbeError::other(format!("failed to parse live FDT: {err:?}")))
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

#[derive(Clone, Copy)]
struct Rk3588ResetPin {
    bank: u8,
    pin: u8,
    active_high: bool,
}

fn rk3588_pcie_reset_pin(apb_base: u64) -> Option<Rk3588ResetPin> {
    match apb_base {
        0xfe18_0000 => Some(Rk3588ResetPin {
            bank: 3,
            pin: 11,
            active_high: true,
        }),
        0xfe19_0000 => Some(Rk3588ResetPin {
            bank: 4,
            pin: 2,
            active_high: true,
        }),
        _ => None,
    }
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

fn program_memory_windows(
    host: &Rk3588PcieHost,
    ranges: &[PciRange],
    cfg_phys: u64,
    cfg_size: u64,
) {
    let mut region = MEM_ATU_FIRST_REGION;
    for range in ranges {
        if is_config_range(range, cfg_phys, cfg_size) {
            continue;
        }
        match range.space {
            PciSpace::Memory32 | PciSpace::Memory64 => {
                let window = OutboundWindow {
                    cpu_base: range.cpu_address,
                    pci_base: range.bus_address,
                    size: range.size,
                };
                if let Err(err) = host.program_memory_window(region, window) {
                    warn!(
                        "PCIe host {:#x}: invalid outbound iATU region {}: {err:?}",
                        host.apb_phys(),
                        region
                    );
                }
                debug!(
                    "PCIe host {:#x}: iATU mem region {} cpu={:#x} pci={:#x} size={:#x}",
                    host.apb_phys(),
                    region,
                    range.cpu_address,
                    range.bus_address,
                    range.size
                );
                region = region.saturating_add(1);
            }
            PciSpace::IO => {}
        }
    }
}

fn log_direct_endpoint(host: &Rk3588PcieHost) {
    if let Some(endpoint) = host.direct_endpoint_info() {
        info!(
            "PCIe endpoint: {} {:04x}:{:04x} (rev {:02x}, class {:02x}{:02x}{:02x})",
            endpoint.address,
            endpoint.vendor_id,
            endpoint.device_id,
            endpoint.revision_id,
            endpoint.base_class,
            endpoint.sub_class,
            endpoint.prog_if
        );
    }
}

fn is_config_range(range: &PciRange, cfg_phys: u64, cfg_size: u64) -> bool {
    range.cpu_address == cfg_phys && range.size == cfg_size
}

fn set_rk3588_bar_range(drv: &mut PcieController, range: &PciRange) {
    super::set_pcie_mem_range(drv, range);
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

mod rk3588_pcie_slot0 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host slot0",
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
        probe_rk3588(info, plat_dev)
    }
}

mod rk3588_pcie_slot1 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host slot1",
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
        probe_rk3588(info, plat_dev)
    }
}

mod rk3588_pcie_slot2 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host slot2",
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
        probe_rk3588(info, plat_dev)
    }
}

mod rk3588_pcie_slot3 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host slot3",
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
        probe_rk3588(info, plat_dev)
    }
}

mod rk3588_pcie_slot4 {
    use super::*;

    module_driver!(
        name: "Rockchip RK3588 PCIe host slot4",
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
        probe_rk3588(info, plat_dev)
    }
}
