extern crate alloc;

use alloc::{format, string::ToString, vec::Vec};
use core::ptr::NonNull;

use crab_usb::{EhciNewParams, usb_if::Speed};
use fdt_edit::{Fdt, Node, NodeType, Phandle, RegFixed};
use log::{debug, info, warn};
use rdrive::{
    probe::{
        OnProbeError,
        fdt::{ClockLine, ResetLine, apply_assigned_clocks, clock_lines, reset_lines},
    },
    register::{FdtInfo, ProbeFdt},
};

use super::{ProbeFdtUsbHost, usb_kernel};
use crate::mmio::iomap;

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

struct EhciResources {
    ctrl: RegFixed,
    clocks: Vec<ClockLine>,
    resets: Vec<ResetLine>,
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let fdt = live_fdt()?;
    let resources = collect_resources(info, &fdt)?;

    enable_clocks(&resources.clocks)?;
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

    let mut clocks = info.clock_lines()?;
    let mut resets = parse_resets(info.node)?;

    for phy in collect_usb2_phys(info.node.as_node(), fdt) {
        apply_assigned_clocks(phy.port)?;
        clocks.extend(clock_lines(phy.port)?);
        apply_assigned_clocks(phy.parent)?;
        clocks.extend(clock_lines(phy.parent)?);
        resets.extend(parse_resets(phy.parent)?);
    }

    Ok(EhciResources {
        ctrl,
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

fn parse_resets(node: NodeType<'_>) -> Result<Vec<ResetLine>, OnProbeError> {
    reset_lines(node)
}

fn enable_clocks(clocks: &[ClockLine]) -> Result<(), OnProbeError> {
    for clock in clocks {
        let id = clock.id().raw();
        if id == 0 {
            continue;
        }

        clock.enable()?;
        debug!("EHCI clock {:?} ({id:#x}) enabled", clock.name());
    }
    Ok(())
}

fn deassert_resets(resets: &[ResetLine]) {
    for reset in resets {
        match reset.deassert() {
            Ok(()) => debug!(
                "EHCI reset {:?} ({:#x}) deasserted",
                reset.name(),
                reset.id().raw()
            ),
            Err(err) => warn!(
                "EHCI reset {:?} ({:#x}) deassert skipped: {err}",
                reset.name(),
                reset.id().raw()
            ),
        }
    }
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
