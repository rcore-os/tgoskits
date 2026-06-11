extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use crab_usb::{
    DwcNewParams, DwcParams, UdphyParam, Usb2PhyParam, Usb2PhyPortId, UsbPhyInterfaceMode,
    usb_if::DrMode,
};
use fdt_edit::{ClockRef, Fdt, Node, NodeType, Phandle, RegFixed};
use log::{debug, info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use rockchip_pm::{PowerDomain, RockchipPM};

use super::{ProbeFdtUsbHost, usb_kernel};
use crate::{
    mmio::iomap,
    soc::{RockchipPinCtrl, rk3588_enable_clock, rk3588_reset_assert, rk3588_reset_deassert},
};

const DRIVER_NAME: &str = "usb-dwc-xhci";
const OPTIONAL_PHP_POWER_DOMAIN: usize = 32;

crate::model_register!(
    name: "USB DWC xHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["snps,dwc3"],
            on_probe: probe
        }
    ],
);

struct RockchipCru;

impl crab_usb::CruOp for RockchipCru {
    fn reset_assert(&self, id: u64) {
        if let Err(err) = rk3588_reset_assert(id) {
            warn!("failed to assert RK3588 reset {id:#x}: {err}");
        }
    }

    fn reset_deassert(&self, id: u64) {
        if let Err(err) = rk3588_reset_deassert(id) {
            warn!("failed to deassert RK3588 reset {id:#x}: {err}");
        }
    }
}

struct ResetSpec {
    name: String,
    id: u64,
}

#[derive(Clone)]
struct ClockSpec {
    name: Option<String>,
    id: u32,
}

struct Usb2PhyResources {
    port_name: String,
    reg: usize,
    grf: Phandle,
    supply: Option<Phandle>,
    resets: Vec<ResetSpec>,
    clocks: Vec<ClockSpec>,
}

struct UsbdpPhyResources {
    id: usize,
    reg: RegFixed,
    u2phy_grf: Phandle,
    usb_grf: Phandle,
    usbdpphy_grf: Phandle,
    vo_grf: Phandle,
    dp_lane_mux: Vec<u32>,
    resets: Vec<ResetSpec>,
    clocks: Vec<ClockSpec>,
}

struct DwcResources {
    ctrl: RegFixed,
    power_domains: Vec<usize>,
    clocks: Vec<ClockSpec>,
    ctrl_resets: Vec<ResetSpec>,
    usb2: Usb2PhyResources,
    usbdp: UsbdpPhyResources,
    params: DwcParams,
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    match prop_str(info.node.as_node(), "dr_mode") {
        Some("host") => {}
        Some(mode) => {
            debug!("skip DWC3 node {} because dr_mode={mode}", info.node.name());
            return Err(OnProbeError::NotMatch);
        }
        None => {
            debug!(
                "skip DWC3 node {} because dr_mode is missing",
                info.node.name()
            );
            return Err(OnProbeError::NotMatch);
        }
    }

    let fdt = live_fdt()?;
    let resources = collect_resources(info, &fdt)?;

    enable_power_domains(&resources.power_domains)?;
    enable_clocks(&resources.clocks);
    enable_vbus(resources.usb2.supply)?;

    let ctrl = map_reg(resources.ctrl)?;
    let phy = map_reg(resources.usbdp.reg)?;
    let u2phy_grf = map_phandle_reg(&fdt, resources.usbdp.u2phy_grf, "rockchip,u2phy-grf")?;
    let usb_grf = map_phandle_reg(&fdt, resources.usbdp.usb_grf, "rockchip,usb-grf")?;
    let usbdpphy_grf =
        map_phandle_reg(&fdt, resources.usbdp.usbdpphy_grf, "rockchip,usbdpphy-grf")?;
    let vo_grf = map_phandle_reg(&fdt, resources.usbdp.vo_grf, "rockchip,vo-grf")?;
    let usb2phy_grf = map_phandle_reg(&fdt, resources.usb2.grf, "usb2phy-grf")?;

    let ctrl_resets = reset_refs(&resources.ctrl_resets);
    let usbdp_resets = reset_refs(&resources.usbdp.resets);
    let usb2_resets = reset_refs(&resources.usb2.resets);
    let usb2_port = Usb2PhyPortId::from_node_name(&resources.usb2.port_name).ok_or_else(|| {
        OnProbeError::other(format!(
            "unsupported USB2 PHY port name {}",
            resources.usb2.port_name
        ))
    })?;

    let host = crab_usb::USBHost::new_dwc(DwcNewParams {
        ctrl,
        phy,
        phy_param: UdphyParam {
            id: resources.usbdp.id,
            u2phy_grf,
            usb_grf,
            usbdpphy_grf,
            vo_grf,
            dp_lane_mux: &resources.usbdp.dp_lane_mux,
            rst_list: &usbdp_resets,
        },
        usb2_phy_param: Usb2PhyParam {
            reg: resources.usb2.reg,
            port_kind: usb2_port,
            usb_grf: usb2phy_grf,
            rst_list: &usb2_resets,
        },
        cru: RockchipCru,
        rst_list: &ctrl_resets,
        params: resources.params,
        kernel: usb_kernel(),
    })
    .map_err(|err| {
        OnProbeError::other(format!(
            "failed to create DWC xHCI host for [{}]: {err}",
            info.node.name()
        ))
    })?;

    let node_name = probe.info().node.name().to_string();
    let irq = probe.register_usb_host(DRIVER_NAME, host)?;
    info!(
        "DWC xHCI driver initialized successfully for {} with irq {:?}",
        node_name, irq
    );
    Ok(())
}

fn collect_resources(info: &FdtInfo<'_>, fdt: &Fdt) -> Result<DwcResources, OnProbeError> {
    let ctrl = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let (usb2_port, usbdp_port) = parse_phys(info.node.as_node())?;
    let usb2 = collect_usb2_phy(fdt, usb2_port)?;
    let usbdp = collect_usbdp_phy(fdt, usbdp_port)?;

    let mut clocks = Vec::new();
    if let Some(parent) = info.node.parent() {
        clocks.extend(clock_specs(parent.clocks()));
    }
    clocks.extend(clock_specs(info.node.clocks()));
    clocks.extend(usb2.clocks.iter().cloned());
    clocks.extend(usbdp.clocks.iter().cloned());

    Ok(DwcResources {
        ctrl,
        power_domains: parse_power_domains(info.node.as_node())?,
        clocks,
        ctrl_resets: parse_resets(info.node)?,
        usb2,
        usbdp,
        params: parse_dwc_params(info.node.as_node()),
    })
}

fn collect_usb2_phy(fdt: &Fdt, port_phandle: Phandle) -> Result<Usb2PhyResources, OnProbeError> {
    let port = fdt
        .get_by_phandle(port_phandle)
        .ok_or_else(|| OnProbeError::other(format!("USB2 PHY port {port_phandle:?} not found")))?;
    let phy = port.parent().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no USB2 PHY parent node", port.name()))
    })?;
    let grf = phy
        .parent()
        .and_then(|node| node.as_node().phandle())
        .ok_or_else(|| {
            OnProbeError::other(format!("[{}] has no USB2 PHY GRF parent", phy.name()))
        })?;
    let phy_reg = phy
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", phy.name())))?;
    let reg = phy_reg.child_bus_address as usize;

    Ok(Usb2PhyResources {
        port_name: port.name().to_string(),
        reg,
        grf,
        supply: get_phandle_prop(port.as_node(), "phy-supply"),
        resets: parse_resets(phy)?,
        clocks: clock_specs(phy.clocks()),
    })
}

fn collect_usbdp_phy(fdt: &Fdt, port_phandle: Phandle) -> Result<UsbdpPhyResources, OnProbeError> {
    let port = fdt
        .get_by_phandle(port_phandle)
        .ok_or_else(|| OnProbeError::other(format!("USBDP PHY port {port_phandle:?} not found")))?;
    let phy = port.parent().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no USBDP PHY parent node", port.name()))
    })?;
    let reg = phy
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", phy.name())))?;

    Ok(UsbdpPhyResources {
        id: usbdp_phy_id(fdt, phy)?,
        reg,
        u2phy_grf: required_phandle_prop(phy.as_node(), "rockchip,u2phy-grf", phy.name())?,
        usb_grf: required_phandle_prop(phy.as_node(), "rockchip,usb-grf", phy.name())?,
        usbdpphy_grf: required_phandle_prop(phy.as_node(), "rockchip,usbdpphy-grf", phy.name())?,
        vo_grf: required_phandle_prop(phy.as_node(), "rockchip,vo-grf", phy.name())?,
        dp_lane_mux: phy
            .as_node()
            .get_property("rockchip,dp-lane-mux")
            .map(|prop| prop.get_u32_iter().collect())
            .unwrap_or_default(),
        resets: parse_resets(phy)?,
        clocks: clock_specs(phy.clocks()),
    })
}

fn parse_phys(node: &Node) -> Result<(Phandle, Phandle), OnProbeError> {
    let phys = node
        .get_property("phys")
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no phys", node.name())))?
        .get_u32_iter()
        .map(Phandle::from)
        .collect::<Vec<_>>();
    if phys.len() < 2 {
        return Err(OnProbeError::other(format!(
            "[{}] needs both USB2 and USB3 PHY phandles",
            node.name()
        )));
    }
    Ok((phys[0], phys[1]))
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

fn parse_resets(node: NodeType<'_>) -> Result<Vec<ResetSpec>, OnProbeError> {
    let Some(resets_prop) = node.as_node().get_property("resets") else {
        return Ok(Vec::new());
    };
    let reset_cells = resets_prop.get_u32_iter().collect::<Vec<_>>();
    if reset_cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed resets",
            node.name()
        )));
    }

    let reset_names = node
        .as_node()
        .get_property("reset-names")
        .ok_or_else(|| {
            OnProbeError::other(format!("[{}] has resets but no reset-names", node.name()))
        })?
        .as_str_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let reset_count = reset_cells.len() / 2;
    if reset_names.len() < reset_count {
        return Err(OnProbeError::other(format!(
            "[{}] has fewer reset-names than resets",
            node.name()
        )));
    }

    Ok(reset_cells
        .chunks(2)
        .zip(reset_names.iter())
        .map(|(cells, name)| ResetSpec {
            name: name.clone(),
            id: cells[1] as u64,
        })
        .collect())
}

fn parse_dwc_params(node: &Node) -> DwcParams {
    let mut params = DwcParams {
        dr_mode: DrMode::Host,
        ..DwcParams::default()
    };

    match prop_str(node, "phy_type") {
        Some("utmi") => params.hsphy_mode = UsbPhyInterfaceMode::Utmi,
        Some("utmi_wide") => params.hsphy_mode = UsbPhyInterfaceMode::UtmiWide,
        _ => {}
    }

    params.has_lpm_erratum = has_prop(node, &["snps,has-lpm-erratum"]);
    params.is_utmi_l1_suspend = has_prop(node, &["snps,is-utmi-l1-suspend"]);
    params.disable_scramble_quirk = has_prop(
        node,
        &["snps,disable_scramble_quirk", "snps,disable-scramble-quirk"],
    );
    params.u2exit_lfps_quirk =
        has_prop(node, &["snps,u2exit_lfps_quirk", "snps,u2exit-lfps-quirk"]);
    params.u2ss_inp3_quirk = has_prop(node, &["snps,u2ss_inp3_quirk", "snps,u2ss-inp3-quirk"]);
    params.req_p1p2p3_quirk = has_prop(node, &["snps,req_p1p2p3_quirk", "snps,req-p1p2p3-quirk"]);
    params.del_p1p2p3_quirk = has_prop(node, &["snps,del_p1p2p3_quirk", "snps,del-p1p2p3-quirk"]);
    params.del_phy_power_chg_quirk = has_prop(
        node,
        &[
            "snps,del_phy_power_chg_quirk",
            "snps,del-phy-power-chg-quirk",
            "snps,dis-del-phy-power-chg-quirk",
        ],
    );
    params.lfps_filter_quirk =
        has_prop(node, &["snps,lfps_filter_quirk", "snps,lfps-filter-quirk"]);
    params.rx_detect_poll_quirk = has_prop(
        node,
        &["snps,rx_detect_poll_quirk", "snps,rx-detect-poll-quirk"],
    );
    params.dis_u3_susphy_quirk = has_prop(
        node,
        &["snps,dis_u3_susphy_quirk", "snps,dis-u3-susphy-quirk"],
    );
    params.dis_u2_susphy_quirk = has_prop(
        node,
        &["snps,dis_u2_susphy_quirk", "snps,dis-u2-susphy-quirk"],
    );
    params.dis_u1u2_quirk = has_prop(
        node,
        &[
            "snps,dis_u1u2_quirk",
            "snps,dis-u1-entry-quirk",
            "snps,dis-u2-entry-quirk",
        ],
    );
    params.dis_enblslpm_quirk = has_prop(
        node,
        &["snps,dis_enblslpm_quirk", "snps,dis-enblslpm-quirk"],
    );
    params.dis_u2_freeclk_exists_quirk = has_prop(
        node,
        &[
            "snps,dis-u2-freeclk-exists-quirk",
            "snps,dis_u2_freeclk_exists_quirk",
        ],
    );
    params.tx_de_emphasis_quirk = has_prop(
        node,
        &["snps,tx_de_emphasis_quirk", "snps,tx-de-emphasis-quirk"],
    );

    params
}

fn enable_power_domains(domains: &[usize]) -> Result<(), OnProbeError> {
    let pm = rdrive::get_one::<RockchipPM>()
        .ok_or_else(|| OnProbeError::other("RockchipPM not found for DWC xHCI"))?;
    let mut pm = pm
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock RockchipPM: {err}")))?;

    for &domain in domains {
        pm.power_domain_on(PowerDomain(domain)).map_err(|err| {
            OnProbeError::other(format!("failed to enable power domain {domain}: {err:?}"))
        })?;
        info!("DWC xHCI power domain {domain} enabled");
    }

    if !domains.contains(&OPTIONAL_PHP_POWER_DOMAIN) {
        match pm.power_domain_on(PowerDomain(OPTIONAL_PHP_POWER_DOMAIN)) {
            Ok(()) => info!("DWC xHCI optional PHP power domain enabled"),
            Err(err) => warn!("DWC xHCI optional PHP power domain enable failed: {err:?}"),
        }
    }

    Ok(())
}

fn enable_clocks(clocks: &[ClockSpec]) {
    for clock in clocks {
        if clock.id == 0 {
            continue;
        }

        match rk3588_enable_clock(clock.id) {
            Ok(()) => debug!("DWC xHCI clock {:?} ({:#x}) enabled", clock.name, clock.id),
            Err(err) => warn!(
                "DWC xHCI clock {:?} ({:#x}) enable skipped: {err}",
                clock.name, clock.id
            ),
        }
    }
}

fn enable_vbus(supply: Option<Phandle>) -> Result<(), OnProbeError> {
    let Some(supply) = supply else {
        debug!("DWC xHCI USB2 PHY has no phy-supply; skip VBUS pinctrl");
        return Ok(());
    };

    let pinctrl = rdrive::get_one::<RockchipPinCtrl>()
        .ok_or_else(|| OnProbeError::other("RockchipPinCtrl not found for DWC xHCI VBUS"))?;
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock RockchipPinCtrl: {err}")))?;
    pinctrl.enable_fixed_regulator(supply)
}

fn clock_specs(clocks: Vec<ClockRef>) -> Vec<ClockSpec> {
    clocks
        .into_iter()
        .filter_map(|clock| {
            let id = *clock.specifier.first()?;
            Some(ClockSpec {
                name: clock.name,
                id,
            })
        })
        .collect()
}

fn reset_refs(resets: &[ResetSpec]) -> Vec<(&str, u64)> {
    resets
        .iter()
        .map(|reset| (reset.name.as_str(), reset.id))
        .collect()
}

fn usbdp_phy_id(fdt: &Fdt, phy: NodeType<'_>) -> Result<usize, OnProbeError> {
    let phy_path = phy.path();
    if let Some(aliases) = fdt.all_nodes().find(|node| node.name() == "aliases") {
        for prop in aliases.as_node().properties() {
            let Some(suffix) = prop.name().strip_prefix("usbdp") else {
                continue;
            };
            let Ok(id) = suffix.parse::<usize>() else {
                continue;
            };
            if prop.as_str() == Some(phy_path.as_str()) {
                return Ok(id);
            }
        }
    }

    if phy.name().contains("fed80000") {
        return Ok(0);
    }
    if phy.name().contains("fed90000") {
        return Ok(1);
    }

    Err(OnProbeError::other(format!(
        "failed to resolve USBDP PHY id for {}",
        phy.path()
    )))
}

fn required_phandle_prop(node: &Node, name: &str, context: &str) -> Result<Phandle, OnProbeError> {
    get_phandle_prop(node, name)
        .ok_or_else(|| OnProbeError::other(format!("[{context}] has no {name}")))
}

fn get_phandle_prop(node: &Node, name: &str) -> Option<Phandle> {
    node.get_property(name)
        .and_then(|prop| prop.get_u32())
        .map(Phandle::from)
}

fn prop_str<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.get_property(name).and_then(|prop| prop.as_str())
}

fn has_prop(node: &Node, names: &[&str]) -> bool {
    names.iter().any(|name| node.get_property(name).is_some())
}

fn live_fdt() -> Result<Fdt, OnProbeError> {
    rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))
}

fn map_phandle_reg(
    fdt: &Fdt,
    phandle: Phandle,
    context: &str,
) -> Result<NonNull<u8>, OnProbeError> {
    let node = fdt
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("{context} phandle {phandle:?} not found")))?;
    let reg = node.regs().into_iter().next().ok_or_else(|| {
        OnProbeError::other(format!("[{}] has no reg for {context}", node.name()))
    })?;
    map_reg(reg)
}

fn map_reg(reg: RegFixed) -> Result<NonNull<u8>, OnProbeError> {
    let size = align_up_4k((reg.size.unwrap_or(0x1000) as usize).max(1));
    iomap(reg.address as usize, size)
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
