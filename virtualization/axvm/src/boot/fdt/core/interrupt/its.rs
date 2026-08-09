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
    let configured_paths = profiles
        .iter()
        .map(|profile| profile.node_path.as_str())
        .collect::<std::vec::Vec<_>>();
    let stale_paths = tree
        .inner()
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = tree.inner().node(node_id)?;
            let path = tree.inner().path_of(node_id);
            (node
                .compatibles()
                .any(|compatible| compatible == "arm,gic-v3-its")
                && !configured_paths.contains(&path.as_str()))
            .then_some(path)
        })
        .collect::<std::vec::Vec<_>>();
    for path in stale_paths {
        tree.inner_mut().remove_by_path(&path);
    }

    for profile in profiles {
        let node_id = tree.ensure_path(&profile.node_path)?;
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
        if let Some(phandle) = profile.node_phandle {
            phandle::install(tree, node_id, phandle, "ITS")?;
        }
    }
    Ok(())
}
