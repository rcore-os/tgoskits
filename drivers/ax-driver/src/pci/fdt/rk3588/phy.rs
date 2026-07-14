use alloc::{format, string::ToString, vec::Vec};
use core::time::Duration;

use fdt_edit::{Node, Phandle};
use log::{info, warn};
use rdrive::probe::{OnProbeError, fdt::NodeType};

use super::{
    clocks_reset_gpio::{
        assert_resets, clock_lines_for_node, deassert_resets, enable_clocks, parse_resets,
    },
    resources::{
        BIT_WRITEABLE_SHIFT, CombphyResources, PCIE3PHY_SRAM_INIT_DONE, PHP_GRF_PCIESEL_CON,
        PHY_TYPE_PCIE, Pcie3PhyResources, PhyRef, RK3588_PCIE3PHY_CMN_CON0,
        RK3588_PCIE3PHY_DEFAULT_MODE, RK3588_PCIE3PHY_PHY0_STATUS1, RK3588_PCIE3PHY_PHY1_STATUS1,
        RegMmio,
    },
    windows::{is_compatible, live_fdt, phy_cells, prop_phandle, prop_str_list, prop_u32},
};

pub(super) fn parse_phys(node_type: NodeType<'_>) -> Result<Vec<PhyRef>, OnProbeError> {
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

pub(super) fn init_phys(host_node: NodeType<'_>, phys: &[PhyRef]) -> Result<(), OnProbeError> {
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
        clocks: clock_lines_for_node(phy)?,
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
    assert_resets(&phy.resets)?;
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

    deassert_resets(&phy.resets)?;
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
        clocks: clock_lines_for_node(phy)?,
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
