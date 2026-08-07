//! Interrupt-provider phandle installation and reference repair.

use fdt_edit::{NodeId, Property};

use super::super::tree::FdtTree;
use crate::{AxVmResult, ax_err_type};

pub(super) fn install(
    tree: &mut FdtTree,
    controller: NodeId,
    phandle: u32,
    controller_name: &str,
) -> AxVmResult {
    if let Some(existing) = tree.inner().get_by_phandle(phandle.into())
        && existing.id() != controller
    {
        return Err(ax_err_type!(
            InvalidData,
            std::format!(
                "host {controller_name} phandle {phandle:#x} conflicts with another guest node"
            )
        ));
    }
    let old_phandle = tree
        .inner()
        .node(controller)
        .and_then(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
        })
        .and_then(Property::get_u32);
    if let Some(old_phandle) = old_phandle.filter(|old| *old != phandle) {
        let references = tree
            .inner()
            .iter_node_ids()
            .filter(|node_id| {
                tree.inner().node(*node_id).is_some_and(|node| {
                    ["interrupt-parent", "msi-parent"].into_iter().any(|name| {
                        node.get_property(name).and_then(Property::get_u32) == Some(old_phandle)
                    })
                })
            })
            .collect::<std::vec::Vec<_>>();
        for node_id in references {
            for name in ["interrupt-parent", "msi-parent"] {
                let matches = tree
                    .inner()
                    .node(node_id)
                    .and_then(|node| node.get_property(name))
                    .and_then(Property::get_u32)
                    == Some(old_phandle);
                if matches {
                    tree.set_property(node_id, prop_u32(name, phandle))?;
                }
            }
        }
    }
    tree.set_property(controller, prop_u32("phandle", phandle))?;
    tree.set_property(controller, prop_u32("linux,phandle", phandle))
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
