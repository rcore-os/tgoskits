extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use crab_usb::{EhciNewParams, usb_if::Speed};
use fdt_edit::{ClockRef, Fdt, Node, NodeType, Phandle, RegFixed};
use log::{debug, info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};

use super::{ProbeFdtUsbHost, usb_kernel};
use crate::{
    mmio::iomap,
    soc::{rk3588_enable_clock, rk3588_enable_power_domain, rk3588_reset_deassert},
};

const DRIVER_NAME: &str = "usb-rockchip-ehci";

crate::model_register!(
    name: "USB Rockchip EHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-ehci", "generic-ehci"],
            on_probe: probe
        }
    ],
);

#[derive(Clone)]
struct ClockSpec {
    name: Option<String>,
    id: u32,
}

#[derive(Clone)]
struct ResetSpec {
    name: String,
    id: u64,
}

struct EhciResources {
    ctrl: RegFixed,
    power_domains: Vec<usize>,
    clocks: Vec<ClockSpec>,
    resets: Vec<ResetSpec>,
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let fdt = live_fdt()?;
    let resources = collect_resources(info, &fdt)?;

    enable_power_domains(&resources.power_domains)?;
    enable_clocks(&resources.clocks);
    deassert_resets(&resources.resets);

    let mmio = map_reg(resources.ctrl)?;
    let host = crab_usb::USBHost::new_ehci(EhciNewParams {
        mmio,
        kernel: usb_kernel(),
    })
    .map_err(|err| {
        OnProbeError::other(format!(
            "failed to create EHCI host for [{}]: {err}",
            info.node.name()
        ))
    })?;

    let node_name = info.node.name().to_string();
    let irq = probe.register_usb_host_with_root_hub_speed(DRIVER_NAME, host, Speed::High)?;
    info!(
        "Rockchip EHCI driver initialized successfully for {} with irq {:?}",
        node_name, irq
    );
    Ok(())
}

fn collect_resources(info: &FdtInfo<'_>, fdt: &Fdt) -> Result<EhciResources, OnProbeError> {
    let ctrl = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;

    let mut clocks = clock_specs(info.node.clocks());
    let mut resets = parse_resets(info.node)?;

    for phy in collect_usb2_phys(info.node.as_node(), fdt) {
        clocks.extend(clock_specs(phy.port.clocks()));
        clocks.extend(clock_specs(phy.parent.clocks()));
        resets.extend(parse_resets(phy.parent)?);
    }

    Ok(EhciResources {
        ctrl,
        power_domains: parse_power_domains(info.node.as_node())?,
        clocks,
        resets,
    })
}

struct Usb2PhyNode<'a> {
    port: NodeType<'a>,
    parent: NodeType<'a>,
}

fn collect_usb2_phys<'a>(node: &Node, fdt: &'a Fdt) -> Vec<Usb2PhyNode<'a>> {
    let Some(phys) = node.get_property("phys") else {
        return Vec::new();
    };

    phys.get_u32_iter()
        .filter_map(|cell| {
            let port = fdt.get_by_phandle(Phandle::from(cell))?;
            let parent = port.parent()?;
            Some(Usb2PhyNode { port, parent })
        })
        .collect()
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

fn enable_power_domains(domains: &[usize]) -> Result<(), OnProbeError> {
    for &domain in domains {
        rk3588_enable_power_domain(domain).map_err(|err| {
            OnProbeError::other(format!(
                "failed to enable EHCI power domain {domain}: {err}"
            ))
        })?;
        info!("EHCI power domain {domain} enabled");
    }
    Ok(())
}

fn enable_clocks(clocks: &[ClockSpec]) {
    for clock in clocks {
        if clock.id == 0 {
            continue;
        }

        match rk3588_enable_clock(clock.id) {
            Ok(()) => debug!("EHCI clock {:?} ({:#x}) enabled", clock.name, clock.id),
            Err(err) => warn!(
                "EHCI clock {:?} ({:#x}) enable skipped: {err}",
                clock.name, clock.id
            ),
        }
    }
}

fn deassert_resets(resets: &[ResetSpec]) {
    for reset in resets {
        match rk3588_reset_deassert(reset.id) {
            Ok(()) => debug!("EHCI reset {} ({:#x}) deasserted", reset.name, reset.id),
            Err(err) => warn!(
                "EHCI reset {} ({:#x}) deassert skipped: {err}",
                reset.name, reset.id
            ),
        }
    }
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

fn live_fdt() -> Result<Fdt, OnProbeError> {
    rdrive::with_fdt(Clone::clone).ok_or_else(|| OnProbeError::other("live FDT not found"))
}

fn map_reg(reg: RegFixed) -> Result<NonNull<u8>, OnProbeError> {
    let size = align_up_4k((reg.size.unwrap_or(0x1000) as usize).max(1));
    iomap(reg.address as usize, size)
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}
