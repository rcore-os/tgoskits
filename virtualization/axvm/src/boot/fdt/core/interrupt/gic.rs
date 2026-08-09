//! ARM GIC firmware parsing and guest register installation.

use fdt_edit::{Fdt, NodeId, Property};
use fdt_raw::RegInfo;

use super::{
    super::tree::FdtTree,
    its, phandle,
    phandle::{prop_string, prop_u32, prop_u64},
};
use crate::{machine::*, *};

const DEFAULT_REDISTRIBUTOR_STRIDE: usize = 0x2_0000;

/// Reads the host GIC register windows and firmware identity.
pub(crate) fn host_gic_profile(fdt: &Fdt) -> AxVmResult<Option<GuestGicProfile>> {
    let Some((controller, compatible)) = fdt.iter_node_ids().find_map(|node_id| {
        let node = fdt.node(node_id)?;
        if node.get_property("interrupt-controller").is_none() {
            return None;
        }
        node.compatibles()
            .find(|compatible| is_supported(compatible))
            .map(|compatible| (node_id, std::string::String::from(compatible)))
    }) else {
        return Ok(None);
    };
    let view = fdt
        .view_typed(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host GIC node is missing"))?;
    let regs = view.regs();
    if regs.len() < 2 {
        return Err(ax_err_type!(
            InvalidData,
            "host GIC node must provide distributor and per-CPU ranges"
        ));
    }
    let (distributor_base, distributor_length) = checked_reg(&regs[0], "distributor")?;
    let node = view.as_node();
    let is_v3 = node
        .compatibles()
        .any(|compatible| compatible == "arm,gic-v3");
    let cpu_region = if is_v3 {
        let region_count = node
            .get_property("#redistributor-regions")
            .and_then(Property::get_u32)
            .map(usize::try_from)
            .transpose()
            .map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    "host GIC #redistributor-regions does not fit usize"
                )
            })?
            .unwrap_or(1);
        if region_count == 0 || region_count >= regs.len() {
            return Err(ax_err_type!(
                InvalidData,
                std::format!(
                    "host GIC declares {region_count} Redistributor regions but provides {} \
                     per-CPU register ranges",
                    regs.len() - 1
                )
            ));
        }
        let regions = regs[1..=region_count]
            .iter()
            .map(|reg| {
                checked_reg(reg, "redistributor")
                    .map(|(base, length)| GuestMmioRegion { base, length })
            })
            .collect::<Result<std::vec::Vec<_>, _>>()?;
        let stride = node
            .get_property("redistributor-stride")
            .and_then(Property::get_u64)
            .map(usize::try_from)
            .transpose()
            .map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    "host GIC redistributor-stride does not fit usize"
                )
            })?
            .unwrap_or(DEFAULT_REDISTRIBUTOR_STRIDE);
        GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile { regions, stride })
    } else {
        let (base, length) = checked_reg(&regs[1], "CPU interface")?;
        GuestGicCpuRegion::CpuInterface(GuestMmioRegion { base, length })
    };
    let its = if is_v3 {
        its::host_profiles(fdt)?
    } else {
        std::vec![]
    };

    Ok(Some(GuestGicProfile {
        compatible,
        node_path: fdt.path_of(controller),
        node_phandle: node
            .get_property("phandle")
            .or_else(|| node.get_property("linux,phandle"))
            .and_then(Property::get_u32),
        distributor: GuestMmioRegion {
            base: distributor_base,
            length: distributor_length,
        },
        cpu_region,
        its,
    }))
}

/// Reads the host VGIC maintenance PPI INTID.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn host_gic_maintenance_intid(fdt: &Fdt) -> AxVmResult<Option<u32>> {
    let Some(controller) = find_in_fdt(fdt) else {
        return Ok(None);
    };
    let node = fdt
        .node(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host GIC node is missing"))?;
    if node
        .get_property("#interrupt-cells")
        .and_then(Property::get_u32)
        != Some(3)
    {
        return Err(ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt requires three interrupt cells"
        ));
    }
    let Some(interrupts) = node.get_property("interrupts") else {
        return Ok(None);
    };
    let mut cells = interrupts.as_reader();
    let interrupt_type = cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt type is missing"
        )
    })?;
    let source = cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt source is missing"
        )
    })?;
    cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt flags are missing"
        )
    })?;

    if interrupt_type != 1 || source >= 16 {
        return Err(ax_err_type!(
            Unsupported,
            std::format!(
                "host GIC maintenance interrupt must be a PPI, got type {interrupt_type} source \
                 {source}"
            )
        ));
    }
    Ok(Some(16 + source))
}

pub(super) fn checked_reg(reg: &fdt_edit::RegFixed, name: &str) -> AxVmResult<(usize, usize)> {
    let base = usize::try_from(reg.address).map_err(|_| {
        ax_err_type!(
            InvalidData,
            std::format!("host GIC {name} address does not fit usize")
        )
    })?;
    let length = reg
        .size
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                std::format!("host GIC {name} range has no size")
            )
        })
        .and_then(|length| {
            usize::try_from(length).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    std::format!("host GIC {name} range size does not fit usize")
                )
            })
        })?;
    if length == 0 {
        return Err(ax_err_type!(
            InvalidData,
            std::format!("host GIC {name} range is empty")
        ));
    }
    Ok((base, length))
}

pub(super) fn install_registers(tree: &mut FdtTree, profile: &GuestGicProfile) -> AxVmResult {
    match (&*profile.compatible, &profile.cpu_region) {
        ("arm,gic-v3", GuestGicCpuRegion::Redistributors(_))
        | ("arm,cortex-a15-gic" | "arm,gic-400", GuestGicCpuRegion::CpuInterface(_)) => {}
        _ => {
            return Err(ax_err_type!(
                InvalidData,
                "guest GIC compatible string does not match its per-CPU register model"
            ));
        }
    }
    let controller = (!profile.node_path.is_empty())
        .then(|| tree.inner().get_by_path_id(&profile.node_path))
        .flatten()
        .or_else(|| find(tree))
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no GIC interrupt-controller node"
            )
        })?;
    let mut registers = std::vec![RegInfo::new(
        profile.distributor.base as u64,
        Some(profile.distributor.length as u64),
    )];
    match &profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(region) => {
            registers.push(RegInfo::new(region.base as u64, Some(region.length as u64)))
        }
        GuestGicCpuRegion::Redistributors(redistributors) => {
            registers.extend(
                redistributors
                    .regions
                    .iter()
                    .map(|region| RegInfo::new(region.base as u64, Some(region.length as u64))),
            );
        }
    };
    tree.inner_mut()
        .view_typed_mut(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "guest GIC node is missing"))?
        .set_regs(&registers);
    tree.set_property(controller, prop_string("compatible", &profile.compatible))?;
    tree.set_property(controller, prop_u32("#interrupt-cells", 3))?;
    match &profile.cpu_region {
        GuestGicCpuRegion::Redistributors(redistributors) => {
            tree.set_property(
                controller,
                prop_u64("redistributor-stride", redistributors.stride as u64),
            )?;
            let region_count = u32::try_from(redistributors.regions.len()).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    "guest GIC Redistributor region count does not fit u32"
                )
            })?;
            if region_count > 1 {
                tree.set_property(controller, prop_u32("#redistributor-regions", region_count))?;
            } else {
                tree.inner_mut()
                    .node_mut(controller)
                    .ok_or_else(|| ax_err_type!(InvalidData, "guest GIC node is missing"))?
                    .remove_property("#redistributor-regions");
            }
        }
        GuestGicCpuRegion::CpuInterface(_) => {
            let node = tree
                .inner_mut()
                .node_mut(controller)
                .ok_or_else(|| ax_err_type!(InvalidData, "guest GIC node is missing"))?;
            node.remove_property("redistributor-stride");
            node.remove_property("#redistributor-regions");
        }
    }
    if let Some(phandle) = profile.node_phandle {
        phandle::install(tree, controller, phandle, "GIC")?;
    }
    its::install_registers(tree, &profile.its)
}

fn find(tree: &FdtTree) -> Option<NodeId> {
    find_in_fdt(tree.inner())
}

fn find_in_fdt(fdt: &Fdt) -> Option<NodeId> {
    fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.get_property("interrupt-controller").is_some()
                && node.compatibles().any(is_supported)
        })
    })
}

fn is_supported(compatible: &str) -> bool {
    matches!(
        compatible,
        "arm,gic-v3" | "arm,cortex-a15-gic" | "arm,gic-400"
    )
}
