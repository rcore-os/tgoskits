//! Guest-local fixed-clock nodes used by resolved virtual hardware.

use alloc::{format, vec::Vec};

use fdt_edit::{Fdt, Node, Property};

use crate::machine::{MachinePlanError, MachinePlanResult};

pub(super) fn add_fixed_clock(guest: &mut Fdt, frequency: u32) -> MachinePlanResult<u32> {
    let phandle = next_phandle(guest);
    let root = guest.root_id();
    let clock = guest.add_node(root, Node::new(&format!("clock-{phandle:x}")));
    let clock = guest
        .node_mut(clock)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("new fixed-clock node for phandle {phandle} cannot be updated"),
        })?;
    clock.set_property(string_property("compatible", "fixed-clock"));
    clock.set_property(u32_list_property("#clock-cells", &[0]));
    clock.set_property(u32_list_property("clock-frequency", &[frequency]));
    clock.set_property(u32_list_property("phandle", &[phandle]));
    Ok(phandle)
}

pub(super) fn next_phandle(fdt: &Fdt) -> u32 {
    fdt.iter_node_ids()
        .filter_map(|node| fdt.node(node))
        .flat_map(|node| {
            [
                node.get_property("phandle"),
                node.get_property("linux,phandle"),
            ]
        })
        .flatten()
        .filter_map(Property::get_u32)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1)
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}

fn u32_list_property(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}
