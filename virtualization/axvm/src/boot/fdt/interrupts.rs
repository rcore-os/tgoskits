//! Checked transformations for guest device-tree interrupt properties.

use alloc::{format, string::String, vec::Vec};

use fdt_edit::{Fdt, NodeId, Property};

use crate::{AxVmResult, ax_err_type};

struct InterruptEntries {
    cells_per_entry: usize,
    specifiers: Vec<u32>,
    names: Option<Vec<String>>,
}

/// Retains the first `retained_count` interrupt entries on compatible nodes.
///
/// The entry width is read from each node's effective interrupt parent. When
/// `interrupt-names` is present, it is truncated to the same number of entries.
/// Nodes and properties unrelated to the selected compatibility string are
/// preserved.
///
/// # Errors
///
/// Returns an error when the DTB cannot be parsed, a selected node has no valid
/// interrupt parent, its interrupt properties are malformed, or fewer than
/// `retained_count` complete entries are available.
pub fn retain_compatible_interrupt_entries(
    fdt_bytes: &[u8],
    compatible: &str,
    retained_count: usize,
) -> AxVmResult<Vec<u8>> {
    let mut fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse FDT: {err:#?}")))?;
    let matching_nodes = fdt
        .find_compatible(&[compatible])
        .into_iter()
        .map(|node| node.id())
        .collect::<Vec<_>>();

    for node_id in matching_nodes {
        retain_node_interrupt_entries(&mut fdt, node_id, retained_count)?;
    }

    Ok(fdt.encode().as_ref().to_vec())
}

fn retain_node_interrupt_entries(
    fdt: &mut Fdt,
    node_id: NodeId,
    retained_count: usize,
) -> AxVmResult {
    let node_path = fdt.path_of(node_id);
    let entries = read_interrupt_entries(fdt, node_id, &node_path)?;
    let retained_cells = entries.retained_cells(retained_count, &node_path)?;
    let retained_names = entries.retained_names(retained_count, &node_path)?;
    let node = fdt
        .node_mut(node_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;

    let mut interrupts = Property::new("interrupts", Vec::new());
    interrupts.set_u32_ls(retained_cells);
    node.set_property(interrupts);

    if let Some(names) = retained_names {
        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        let mut interrupt_names = Property::new("interrupt-names", Vec::new());
        interrupt_names.set_string_ls(&name_refs);
        node.set_property(interrupt_names);
    }
    Ok(())
}

fn read_interrupt_entries(
    fdt: &Fdt,
    node_id: NodeId,
    node_path: &str,
) -> AxVmResult<InterruptEntries> {
    let node = fdt
        .node(node_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;
    let interrupt_parent = fdt
        .view_typed(node_id)
        .and_then(|view| view.interrupt_parent())
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("FDT node {node_path} has no effective interrupt-parent")
            )
        })?;
    let cells_per_entry = fdt
        .get_by_phandle(interrupt_parent)
        .and_then(|provider| provider.as_node().get_property("#interrupt-cells"))
        .and_then(Property::get_u32)
        .filter(|cells| *cells != 0)
        .map(|cells| cells as usize)
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!(
                    "FDT node {node_path} references an interrupt controller without a valid \
                     #interrupt-cells"
                )
            )
        })?;
    let interrupts = node.get_property("interrupts").ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("FDT node {node_path} has no interrupts property")
        )
    })?;
    if interrupts.data.len() % core::mem::size_of::<u32>() != 0 {
        return Err(ax_err_type!(
            InvalidData,
            format!("FDT node {node_path} has a non-cell-aligned interrupts property")
        ));
    }

    let specifiers = interrupts.get_u32_iter().collect::<Vec<_>>();
    if specifiers.len() % cells_per_entry != 0 {
        return Err(ax_err_type!(
            InvalidData,
            format!(
                "FDT node {node_path} has an incomplete interrupt specifier: {} cells for \
                 {cells_per_entry}-cell entries",
                specifiers.len()
            )
        ));
    }
    let entry_count = specifiers.len() / cells_per_entry;
    let names = node
        .get_property("interrupt-names")
        .map(|property| parse_interrupt_names(property, entry_count, node_path))
        .transpose()?;

    Ok(InterruptEntries {
        cells_per_entry,
        specifiers,
        names,
    })
}

impl InterruptEntries {
    fn retained_cells(&self, retained_count: usize, node_path: &str) -> AxVmResult<&[u32]> {
        let retained_cell_count = retained_count
            .checked_mul(self.cells_per_entry)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("FDT node {node_path} interrupt entry count overflows")
                )
            })?;
        self.specifiers.get(..retained_cell_count).ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!(
                    "FDT node {node_path} contains only {} interrupt entries, cannot retain \
                     {retained_count}",
                    self.specifiers.len() / self.cells_per_entry
                )
            )
        })
    }

    fn retained_names(
        &self,
        retained_count: usize,
        node_path: &str,
    ) -> AxVmResult<Option<&[String]>> {
        self.names
            .as_ref()
            .map(|names| {
                names.get(..retained_count).ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        format!(
                            "FDT node {node_path} contains only {} interrupt names, cannot retain \
                             {retained_count}",
                            names.len()
                        )
                    )
                })
            })
            .transpose()
    }
}

fn parse_interrupt_names(
    property: &Property,
    entry_count: usize,
    node_path: &str,
) -> AxVmResult<Vec<String>> {
    let terminator_count = property.data.iter().filter(|byte| **byte == 0).count();
    let names = property.as_str_iter().map(String::from).collect::<Vec<_>>();
    if property.data.last() != Some(&0)
        || names.len() != terminator_count
        || names.len() != entry_count
    {
        return Err(ax_err_type!(
            InvalidData,
            format!(
                "FDT node {node_path} has {} valid interrupt names for {entry_count} entries",
                names.len()
            )
        ));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use fdt_edit::{Fdt, Node, Property};

    use super::retain_compatible_interrupt_entries;

    #[test]
    fn retains_complete_interrupt_entries_and_names() {
        let dtb = timer_dtb(&[
            1, 13, 4, // secure physical timer
            1, 14, 4, // non-secure physical timer
            1, 11, 4, // virtual timer
            1, 10, 4, // hypervisor timer
        ]);

        let bytes = retain_compatible_interrupt_entries(&dtb, "arm,armv8-timer", 2).unwrap();

        let reparsed = Fdt::from_bytes(&bytes).unwrap();
        let timer = reparsed.get_by_path("/timer").unwrap().as_node();
        let interrupts = timer
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>();
        let interrupt_names = timer
            .get_property("interrupt-names")
            .unwrap()
            .as_str_iter()
            .collect::<Vec<_>>();

        assert_eq!(interrupts, [1, 13, 4, 1, 14, 4]);
        assert_eq!(interrupt_names, ["sec-phys", "phys"]);
    }

    #[test]
    fn rejects_incomplete_interrupt_entries() {
        let dtb = timer_dtb(&[
            1, 13, 4, // secure physical timer
            1, 14, 4, // non-secure physical timer
            1, 11, // incomplete virtual timer specifier
        ]);

        assert!(retain_compatible_interrupt_entries(&dtb, "arm,armv8-timer", 2).is_err());
    }

    fn timer_dtb(interrupts: &[u32]) -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();

        let mut gic = Node::new("interrupt-controller@8000000");
        gic.set_property(Property::new("interrupt-controller", vec![]));
        gic.set_property(prop_u32("#interrupt-cells", 3));
        gic.set_property(prop_u32("phandle", 1));
        fdt.add_node(root, gic);

        let mut timer = Node::new("timer");
        timer.set_property(prop_str("compatible", "arm,armv8-timer"));
        timer.set_property(prop_u32("interrupt-parent", 1));
        let mut interrupt_property = Property::new("interrupts", vec![]);
        interrupt_property.set_u32_ls(interrupts);
        timer.set_property(interrupt_property);
        let mut interrupt_names = Property::new("interrupt-names", vec![]);
        interrupt_names.set_string_ls(&["sec-phys", "phys", "virt", "hyp"]);
        timer.set_property(interrupt_names);
        fdt.add_node(root, timer);

        fdt.encode().as_ref().to_vec()
    }

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut property = Property::new(name, vec![]);
        property.set_u32_ls(&[value]);
        property
    }

    fn prop_str(name: &str, value: &str) -> Property {
        let mut property = Property::new(name, vec![]);
        property.set_string(value);
        property
    }
}
