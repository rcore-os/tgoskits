extern crate alloc;

use alloc::format;
use core::time::Duration;

use fdt_edit::{PciRange, PciSpace, RegFixed};
use mmio_api::{MmioAddr, MmioOp, MmioRaw};
use rdif_pcie::{PciMem64, PcieController};
use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, fdt::NodeType},
    register::FdtInfo,
};
use rk3588_pci::{
    Delay, HostConfig, MEM_ATU_FIRST_REGION, OutboundWindow, ResetControl, Rk3588PcieHost,
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
        self.set_logical(true);
        info!(
            "Rockchip RK3588 PCIe host {:#x}: assert PERST via GPIO{} pin {}",
            self.apb_phys, self.bank, self.pin
        );
    }

    fn deassert_perst(&mut self) {
        self.set_logical(false);
        info!(
            "Rockchip RK3588 PCIe host {:#x}: release PERST after {}ms",
            self.apb_phys, RK3588_PCIE_PERST_INACTIVE_MS
        );
    }
}

fn probe_rk3588(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let node_name = info.node.as_node().name();
    let NodeType::Pci(node) = info.node else {
        return Err(OnProbeError::NotMatch);
    };

    let regs = node.regs();
    let apb_reg = *regs
        .first()
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no APB register")))?;
    let dbi_reg = *regs
        .get(1)
        .ok_or_else(|| OnProbeError::other(format!("{node_name} has no DBI register")))?;

    let ranges = node.ranges().unwrap_or_default();
    let (cfg_phys, cfg_size) = config_window(&regs, &ranges)?;
    let (bus_base, logical_bus_end) = bus_range_info(node.bus_range());
    let mut reset = pcie_reset_gpio(&info, apb_reg.address);

    let apb_size = apb_reg.size.unwrap_or(0x10000) as usize;
    let dbi_size = dbi_reg.size.unwrap_or(0x400000) as usize;
    let apb = map_mmio(apb_reg.address, apb_size)?;
    let dbi = map_mmio(dbi_reg.address, dbi_size)?;
    let cfg = map_mmio(cfg_phys, cfg_size as usize)?;

    let mut host = Rk3588PcieHost::new(
        apb,
        dbi,
        cfg,
        HostConfig {
            apb_phys: apb_reg.address,
            cfg_phys,
            cfg_size: cfg_size as usize,
            bus_base,
            logical_bus_end,
        },
    )
    .map_err(map_rk3588_error)?;

    let delay = AxDelay;
    match reset.as_mut() {
        Some(reset) => {
            host.init(&delay, Some(reset));
        }
        None => {
            host.init(&delay, None);
        }
    }

    program_memory_windows(&host, &ranges, cfg_phys, cfg_size);
    host.unmask_legacy_intx_all();
    info!(
        "Rockchip RK3588 PCIe host {:#x}: legacy INTx unmasked",
        host.apb_phys()
    );
    log_direct_endpoint(&host);
    super::register_legacy_irq(&info, logical_bus_end);

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
        apb_reg.address,
        cfg_phys,
        cfg_size,
        bus_base,
        bus_base.saturating_add(logical_bus_end),
        logical_bus_end
    );
    plat_dev.register_pcie(drv);
    Ok(())
}

fn map_mmio(phys: u64, size: usize) -> Result<MmioRaw, OnProbeError> {
    crate::boot::Kernel
        .ioremap(MmioAddr::from(phys), size)
        .map_err(|err| {
            OnProbeError::other(format!(
                "failed to map MMIO region at {phys:#x} size {size:#x}: {err:?}"
            ))
        })
}

fn map_rk3588_error(err: rk3588_pci::Error) -> OnProbeError {
    OnProbeError::other(format!("{err:?}"))
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

    match Rk3588GpioReset::map(apb_base, default.bank, default.pin, active_high) {
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
        probe_rk3588(info, plat_dev)
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
        probe_rk3588(info, plat_dev)
    }
}
