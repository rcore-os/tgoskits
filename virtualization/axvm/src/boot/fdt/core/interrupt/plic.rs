//! RISC-V PLIC firmware parsing and guest register installation.

use fdt_edit::{Fdt, NodeId, Property};
use fdt_raw::RegInfo;

use super::{super::tree::FdtTree, phandle, phandle::prop_u32};
use crate::{AxVmResult, ax_err_type, machine::GuestPlicProfile};

/// Reads the host PLIC register window and firmware identity.
pub(crate) fn host_plic_profile(fdt: &Fdt) -> AxVmResult<Option<GuestPlicProfile>> {
    let Some(controller) = find_in_fdt(fdt) else {
        return Ok(None);
    };
    let view = fdt
        .view_typed(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC node is missing"))?;
    let reg = view
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC node has no register range"))?;
    let (base, length) = checked_reg(&reg)?;
    let node = view.as_node();

    Ok(Some(GuestPlicProfile {
        node_path: fdt.path_of(controller),
        node_phandle: node
            .get_property("phandle")
            .or_else(|| node.get_property("linux,phandle"))
            .and_then(Property::get_u32),
        base,
        length,
    }))
}

pub(super) fn install_registers(tree: &mut FdtTree, profile: &GuestPlicProfile) -> AxVmResult {
    let controller = tree
        .inner()
        .get_by_path_id(&profile.node_path)
        .or_else(|| find(tree))
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no PLIC interrupt-controller node"
            )
        })?;
    tree.inner_mut()
        .view_typed_mut(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "guest PLIC node is missing"))?
        .set_regs(&[RegInfo::new(
            profile.base as u64,
            Some(profile.length as u64),
        )]);
    tree.set_property(controller, prop_u32("#interrupt-cells", 1))?;
    if let Some(value) = profile.node_phandle {
        phandle::install(tree, controller, value, "PLIC")?;
    }
    Ok(())
}

fn checked_reg(reg: &fdt_edit::RegFixed) -> AxVmResult<(usize, usize)> {
    let base = usize::try_from(reg.address)
        .map_err(|_| ax_err_type!(InvalidData, "host PLIC address does not fit usize"))?;
    let length = reg
        .size
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC range has no size"))
        .and_then(|length| {
            usize::try_from(length)
                .map_err(|_| ax_err_type!(InvalidData, "host PLIC range size does not fit usize"))
        })?;
    if length == 0 {
        return Err(ax_err_type!(InvalidData, "host PLIC range is empty"));
    }
    Ok((base, length))
}

fn find(tree: &FdtTree) -> Option<NodeId> {
    find_in_fdt(tree.inner())
}

fn find_in_fdt(fdt: &Fdt) -> Option<NodeId> {
    fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.get_property("interrupt-controller").is_some()
                && node
                    .compatibles()
                    .any(|compatible| matches!(compatible, "riscv,plic0" | "sifive,plic-1.0.0"))
        })
    })
}
