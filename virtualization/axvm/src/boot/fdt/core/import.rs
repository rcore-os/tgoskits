//! Transactional subtree import between independent firmware namespaces.

use std::{collections::BTreeMap, vec::Vec};

use fdt_edit::{Fdt, Node, NodeId, Property};

use super::{
    references::{phandle_index, reference_offsets},
    tree::{FdtTree, checked_node_phandle, copy_properties},
};
use crate::{AxVmResult, ax_err_type};

/// Copies nodes first, then resolves references in the source namespace.
/// No source handle is ever looked up in the destination tree.
pub(super) fn copy_subtree(
    destination: &mut FdtTree,
    source: &Fdt,
    source_root: NodeId,
    destination_parent: NodeId,
    filter_cpu_properties: bool,
) -> AxVmResult<NodeId> {
    let source_handles = phandle_index(source)?;
    let mut staged = FdtTree::from_fdt(destination.inner().clone());
    staged.validate_phandles()?;
    let mut bindings = BTreeMap::new();
    let mut pending = std::vec![(source_root, destination_parent)];
    while let Some((source_id, parent)) = pending.pop() {
        let node = source
            .node(source_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "source firmware node is missing"))?;
        let path = std::format!(
            "{}/{}",
            staged.inner().path_of(parent).trim_end_matches('/'),
            node.name()
        );
        if staged.inner().get_by_path_id(&path).is_some() {
            return Err(ax_err_type!(
                InvalidData,
                std::format!("firmware import would overwrite {path}")
            ));
        }
        let identity = staged.replacement_phandle(&path, checked_node_phandle(node)?)?;
        let mut copied = Node::new(node.name());
        copy_properties(source, node, &mut copied, filter_cpu_properties);
        copied.remove_property("phandle");
        copied.remove_property("linux,phandle");
        if let Some(handle) = identity {
            copied.set_property(u32_property("phandle", &[handle]));
            copied.set_property(u32_property("linux,phandle", &[handle]));
        }
        let destination_id = staged.add_node(parent, copied);
        bindings.insert(source_id, destination_id);
        for child in node.children().iter().rev() {
            pending.push((*child, destination_id));
        }
    }
    for (&source_id, &destination_id) in &bindings {
        let properties = staged
            .inner()
            .node(destination_id)
            .unwrap()
            .properties()
            .to_vec();
        for property in properties {
            let offsets = match reference_offsets(source, &source_handles, source_id, &property)? {
                Some(offsets) => offsets,
                None if is_value_property(property.name()) => continue,
                None => {
                    return Err(ax_err_type!(
                        Unsupported,
                        std::format!(
                            "cannot import {}:{} without a property binding",
                            source.path_of(source_id),
                            property.name()
                        )
                    ));
                }
            };
            if offsets.is_empty() {
                continue;
            }
            let mut cells = property.get_u32_iter().collect::<Vec<_>>();
            for offset in offsets {
                let source_provider = source_handles.get(&cells[offset]).ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        std::format!(
                            "{}:{} references missing source phandle {:#x}",
                            source.path_of(source_id),
                            property.name(),
                            cells[offset]
                        )
                    )
                })?;
                let destination_provider = bindings.get(source_provider).ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        std::format!(
                            "{}:{} depends on {} outside the imported subtree; an explicit guest \
                             binding is required",
                            source.path_of(source_id),
                            property.name(),
                            source.path_of(*source_provider)
                        )
                    )
                })?;
                cells[offset] = staged.node_phandle(*destination_provider)?.ok_or_else(|| {
                    ax_err_type!(InvalidData, "imported provider has no guest phandle")
                })?;
            }
            staged.set_property(destination_id, u32_property(property.name(), &cells))?;
        }
    }
    let imported_root = bindings[&source_root];
    *destination = staged;
    Ok(imported_root)
}

fn u32_property(name: &str, cells: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(cells);
    property
}

// Import is deliberately closed to unknown bindings: opaque integers might be
// source phandles. Existing guest properties are not passed through this filter.
fn is_value_property(name: &str) -> bool {
    matches!(
        name,
        "phandle"
            | "linux,phandle"
            | "compatible"
            | "device_type"
            | "status"
            | "reg"
            | "reg-names"
            | "ranges"
            | "dma-ranges"
            | "dma-coherent"
            | "interrupts"
            | "interrupt-names"
            | "interrupt-controller"
            | "msi-controller"
            | "gpio-controller"
            | "#address-cells"
            | "#size-cells"
            | "#interrupt-cells"
            | "#clock-cells"
            | "#reset-cells"
            | "#power-domain-cells"
            | "#phy-cells"
            | "#dma-cells"
            | "#iommu-cells"
            | "#io-channel-cells"
            | "#interconnect-cells"
            | "#msi-cells"
            | "#thermal-sensor-cells"
            | "#sound-dai-cells"
            | "#mbox-cells"
            | "#pwm-cells"
            | "#cooling-cells"
            | "#gpio-cells"
            | "enable-method"
            | "capacity-dmips-mhz"
            | "clock-frequency"
            | "clock-names"
            | "clock-output-names"
            | "reset-names"
            | "assigned-clock-rates"
            | "pinctrl-names"
            | "interrupt-map-mask"
            | "interrupt-map-pass-thru"
            | "msi-map-mask"
            | "iommu-map-mask"
            | "bootargs"
            | "stdout-path"
            | "stdin-path"
            | "linux,stdout-path"
            | "linux,initrd-start"
            | "linux,initrd-end"
            | "rng-seed"
            | "kaslr-seed"
            | "linux,pci-probe-only"
    )
}
