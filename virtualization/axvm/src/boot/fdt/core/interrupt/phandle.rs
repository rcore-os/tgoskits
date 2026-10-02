//! Interrupt-provider phandle installation and reference repair.

use fdt_edit::{NodeId, Property};

use super::super::tree::FdtTree;
use crate::{AxVmResult, ax_err_type};

pub(super) fn install(
    tree: &mut FdtTree,
    controller: NodeId,
    preferred: Option<u32>,
) -> AxVmResult {
    let path = tree.inner().path_of(controller);
    let handle = match tree.replacement_phandle(&path, preferred)? {
        Some(handle) => handle,
        None => return Ok(()),
    };
    tree.set_property(controller, prop_u32("phandle", handle))?;
    tree.set_property(controller, prop_u32("linux,phandle", handle))
}

/// Resolves a machine role, not a coincidentally equal host phandle or path.
/// Multiple candidates require an explicit binding rather than first-match selection.
pub(crate) fn controller_node(
    tree: &FdtTree,
    matches: impl Fn(&fdt_edit::Node) -> bool,
    name: &str,
) -> AxVmResult<NodeId> {
    let mut candidates = tree
        .inner()
        .iter_node_ids()
        .filter(|id| tree.inner().node(*id).is_some_and(&matches));
    let node = candidates.next().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            std::format!("guest FDT has no {name} controller")
        )
    })?;
    if candidates.next().is_some() {
        return Err(ax_err_type!(
            InvalidData,
            std::format!("guest FDT has ambiguous {name} controllers")
        ));
    }
    Ok(node)
}

pub(super) fn prop_u32(name: &str, value: u32) -> Property {
    let mut property = Property::new(name, std::vec![]);
    property.set_u32_ls(&[value]);
    property
}

pub(super) fn prop_u64(name: &str, value: u64) -> Property {
    let mut property = Property::new(name, std::vec![]);
    property.set_u64(value);
    property
}

pub(super) fn prop_string(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, std::vec![]);
    property.set_string(value);
    property
}
