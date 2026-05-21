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
    rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))
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
