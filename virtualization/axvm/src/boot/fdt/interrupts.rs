//! Checked transformations for guest device-tree interrupt properties.

use alloc::{format, string::String, vec::Vec};

use fdt_edit::{Fdt, NodeId, Property};

use crate::{AxVmResult, ax_err_type};

struct InterruptEntries {
    cells_per_entry: usize,
    specifiers: Vec<u32>,
    names: Option<Vec<String>>,
}

/// Projects compatible nodes to the emulated EL1 physical-timer capability.
///
/// The entry width is read from each node's effective interrupt parent. When
/// `interrupt-names` is present, the secure and non-secure physical entries are
/// located by role. Otherwise the standard Arm timer binding order is used.
/// The resulting property keeps those entries in positions zero and one and
/// removes `interrupt-names`. This positional pair works with kernels predating
/// named architected-timer interrupts and does not advertise the unimplemented
/// virtual or hypervisor timers.
///
/// # Errors
///
/// Returns an error when the DTB cannot be parsed, a selected node has no valid
/// interrupt parent, its interrupt properties are malformed, or either physical
/// timer entry is absent.
pub fn project_guest_physical_timer_interrupts(
    fdt_bytes: &[u8],
    compatible: &str,
) -> AxVmResult<Vec<u8>> {
    let mut fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse FDT: {err:#?}")))?;
    let matching_nodes = fdt
        .find_compatible(&[compatible])
        .into_iter()
        .map(|node| node.id())
        .collect::<Vec<_>>();

    for node_id in matching_nodes {
        project_node_physical_timer_interrupts(&mut fdt, node_id)?;
    }

    Ok(fdt.encode().as_ref().to_vec())
}

fn project_node_physical_timer_interrupts(fdt: &mut Fdt, node_id: NodeId) -> AxVmResult {
    let node_path = fdt.path_of(node_id);
    let entries = read_interrupt_entries(fdt, node_id, &node_path)?;
    let selected = entries.physical_timer_indices(&node_path)?;
    let retained_cells = entries.selected_cells(selected, &node_path)?;
    let node = fdt
        .node_mut(node_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;
    let mut interrupts = Property::new("interrupts", Vec::new());
    interrupts.set_u32_ls(&retained_cells);
    node.set_property(interrupts);
    node.remove_property("interrupt-names");
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
    fn physical_timer_indices(&self, node_path: &str) -> AxVmResult<[usize; 2]> {
        if let Some(names) = &self.names {
            let secure = names
                .iter()
                .position(|name| matches!(name.as_str(), "sec-phys" | "secure-phys"))
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        format!("FDT node {node_path} has no secure physical timer interrupt")
                    )
                })?;
            let physical = names
                .iter()
                .position(|name| matches!(name.as_str(), "phys" | "non-secure-phys"))
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        format!("FDT node {node_path} has no non-secure physical timer interrupt")
                    )
                })?;
            return Ok([secure, physical]);
        }

        let entry_count = self.specifiers.len() / self.cells_per_entry;
        if entry_count < 2 {
            Err(ax_err_type!(
                InvalidData,
                format!("FDT node {node_path} has fewer than two Arm timer interrupts")
            ))
        } else {
            Ok([0, 1])
        }
    }

    fn selected_cells(&self, indices: [usize; 2], node_path: &str) -> AxVmResult<Vec<u32>> {
        let mut selected = Vec::with_capacity(2 * self.cells_per_entry);
        for index in indices {
            let start = index.checked_mul(self.cells_per_entry).ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("FDT node {node_path} interrupt entry index overflows")
                )
            })?;
            let end = start.checked_add(self.cells_per_entry).ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("FDT node {node_path} interrupt entry end overflows")
                )
            })?;
            selected.extend_from_slice(self.specifiers.get(start..end).ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("FDT node {node_path} has no complete interrupt entry {index}")
                )
            })?);
        }
        Ok(selected)
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

    use super::project_guest_physical_timer_interrupts;

    #[test]
    fn projects_named_timer_entries_to_the_supported_physical_pair() {
        let interrupts = [
            1, 13, 4, // secure physical timer
            1, 14, 4, // non-secure physical timer
            1, 11, 4, // virtual timer
            1, 10, 4, // hypervisor timer
        ];
        let dtb = timer_dtb(&interrupts);

        let bytes = project_guest_physical_timer_interrupts(&dtb, "arm,armv8-timer").unwrap();

        let reparsed = Fdt::from_bytes(&bytes).unwrap();
        let timer = reparsed.get_by_path("/timer").unwrap().as_node();
        let retained = timer
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>();
        assert_eq!(retained, [1, 13, 4, 1, 14, 4]);
        assert!(timer.get_property("interrupt-names").is_none());
    }

    #[test]
    fn projects_physical_timer_without_reindexing_legacy_linux() {
        let interrupts = [
            1, 13, 4, // secure physical timer
            1, 14, 4, // non-secure physical timer
            1, 11, 4, // virtual timer
            1, 10, 4, // hypervisor timer
        ];
        let dtb = timer_dtb_without_names(&interrupts);

        let bytes = project_guest_physical_timer_interrupts(&dtb, "arm,armv8-timer").unwrap();

        let reparsed = Fdt::from_bytes(&bytes).unwrap();
        let timer = reparsed.get_by_path("/timer").unwrap().as_node();
        let retained = timer
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>();
        assert_eq!(retained, [1, 13, 4, 1, 14, 4]);
        assert!(timer.get_property("interrupt-names").is_none());
    }

    #[test]
    fn rejects_incomplete_interrupt_entries() {
        let dtb = timer_dtb_without_names(&[
            1, 13, 4, // secure physical timer
            1, 14, 4, // non-secure physical timer
            1, 11, // incomplete virtual timer specifier
        ]);

        assert!(project_guest_physical_timer_interrupts(&dtb, "arm,armv8-timer").is_err());
    }

    fn timer_dtb(interrupts: &[u32]) -> Vec<u8> {
        timer_dtb_with_names(interrupts, Some(&["sec-phys", "phys", "virt", "hyp"]))
    }

    fn timer_dtb_without_names(interrupts: &[u32]) -> Vec<u8> {
        timer_dtb_with_names(interrupts, None)
    }

    fn timer_dtb_with_names(interrupts: &[u32], names: Option<&[&str]>) -> Vec<u8> {
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
        if let Some(names) = names {
            let mut interrupt_names = Property::new("interrupt-names", vec![]);
            interrupt_names.set_string_ls(names);
            timer.set_property(interrupt_names);
        }
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
