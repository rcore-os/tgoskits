//! ARM GIC ITS firmware parsing and guest register installation.

use axdevice_base::ItsId;
use fdt_edit::{Fdt, Property};
use fdt_raw::RegInfo;

use super::{
    super::tree::FdtTree,
    gic::checked_reg,
    phandle,
    phandle::{prop_string, prop_u32},
};
use crate::{machine::*, *};

pub(super) fn host_profiles(fdt: &Fdt) -> AxVmResult<std::vec::Vec<GuestItsProfile>> {
    let mut nodes = fdt
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = fdt.node(node_id)?;
            node.compatibles()
                .any(|compatible| compatible == "arm,gic-v3-its")
                .then(|| (fdt.path_of(node_id), node_id))
        })
        .collect::<std::vec::Vec<_>>();
    nodes.sort_by(|left, right| left.0.cmp(&right.0));

    nodes
        .into_iter()
        .enumerate()
        .map(|(index, (node_path, node_id))| {
            let view = fdt
                .view_typed(node_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "host ITS node is missing"))?;
            let node = view.as_node();
            if node.get_property("msi-controller").is_none() {
                return Err(ax_err_type!(
                    InvalidData,
                    std::format!("host ITS node {node_path} has no msi-controller property")
                ));
            }
            let regs = view.regs();
            let [reg] = regs.as_slice() else {
                return Err(ax_err_type!(
                    InvalidData,
                    std::format!(
                        "host ITS node {node_path} must have exactly one register range, got {}",
                        regs.len()
                    )
                ));
            };
            let (base, length) = checked_reg(reg, "ITS")?;
            let id = u32::try_from(index).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    "host exposes more ITS instances than u32 can identify"
                )
            })?;
            Ok(GuestItsProfile {
                id: ItsId::new(id),
                node_path,
                node_phandle: node
                    .get_property("phandle")
                    .or_else(|| node.get_property("linux,phandle"))
                    .and_then(Property::get_u32),
                registers: GuestMmioRegion { base, length },
            })
        })
        .collect()
}

pub(super) fn install_registers(tree: &mut FdtTree, profiles: &[GuestItsProfile]) -> AxVmResult {
    let existing = tree
        .inner()
        .iter_node_ids()
        .filter(|id| {
            tree.inner().node(*id).is_some_and(|node| {
                node.compatibles()
                    .any(|compatible| compatible == "arm,gic-v3-its")
            })
        })
        .collect::<std::vec::Vec<_>>();
    let mut selected = std::collections::BTreeSet::new();
    let mut bindings = std::vec::Vec::new();
    for profile in profiles {
        let named = tree.inner().get_by_path_id(&profile.node_path);
        let target = if let Some(named) = named {
            if !existing.contains(&named) {
                return Err(ax_err_type!(
                    InvalidData,
                    std::format!(
                        "ITS path {} belongs to another guest device",
                        profile.node_path
                    )
                ));
            }
            Some(named)
        } else {
            // Identity-mapped physical ITS instances can have different node paths.
            // Register identity is required; ordinal or compatible-only matching is ambiguous.
            let mut candidates = existing.iter().copied().filter(|id| {
                tree.inner().view_typed(*id).is_some_and(|node| {
                    let regs = node.regs();
                    regs.len() == 1
                        && regs[0].address == profile.registers.base as u64
                        && regs[0].size == Some(profile.registers.length as u64)
                })
            });
            let first = candidates.next();
            if candidates.next().is_some() {
                return Err(ax_err_type!(
                    InvalidData,
                    "ambiguous guest ITS register binding"
                ));
            }
            first
        };
        if let Some(target) = target
            && !selected.insert(target)
        {
            return Err(ax_err_type!(
                InvalidData,
                "multiple ITS profiles bind the same guest node"
            ));
        }
        bindings.push((profile, target));
    }
    if existing.iter().any(|id| !selected.contains(id)) {
        return Err(ax_err_type!(
            InvalidData,
            "guest ITS has no matching machine instance; explicit binding is required"
        ));
    }
    for (profile, target) in bindings {
        let node_id = match target {
            Some(id) => id,
            None => tree.ensure_path(&profile.node_path)?,
        };
        tree.inner_mut()
            .view_typed_mut(node_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "guest ITS node is missing"))?
            .set_regs(&[RegInfo::new(
                profile.registers.base as u64,
                Some(profile.registers.length as u64),
            )]);
        tree.set_property(node_id, prop_string("compatible", "arm,gic-v3-its"))?;
        tree.set_property(node_id, Property::new("msi-controller", std::vec![]))?;
        tree.set_property(node_id, prop_u32("#msi-cells", 1))?;
        phandle::install(tree, node_id, profile.node_phandle)?;
    }
    Ok(())
}
