//! Binding-aware firmware references shared by import and resource discovery.

use std::{collections::BTreeMap, vec::Vec};

use fdt_edit::{Fdt, NodeId, Property};

use super::tree::checked_node_phandle;
use crate::{AxVmResult, ax_err_type};

pub(super) fn phandle_index(fdt: &Fdt) -> AxVmResult<BTreeMap<u32, NodeId>> {
    let mut handles = BTreeMap::new();
    for id in fdt.iter_node_ids() {
        if let Some(handle) = checked_node_phandle(fdt.node(id).unwrap())?
            && let Some(previous) = handles.insert(handle, id)
        {
            return Err(ax_err_type!(
                InvalidData,
                std::format!(
                    "duplicate firmware phandle {handle:#x}: {} and {}",
                    fdt.path_of(previous),
                    fdt.path_of(id)
                )
            ));
        }
    }
    Ok(handles)
}

/// Returns only phandle positions, never provider-specific argument cells.
pub(super) fn reference_offsets(
    source: &Fdt,
    handles: &BTreeMap<u32, NodeId>,
    node: NodeId,
    property: &Property,
) -> AxVmResult<Option<Vec<usize>>> {
    let name = property.name();
    let provider_cells = match name {
        "clocks" | "assigned-clocks" | "assigned-clock-parents" => Some("#clock-cells"),
        "resets" => Some("#reset-cells"),
        "power-domains" => Some("#power-domain-cells"),
        "phys" => Some("#phy-cells"),
        "dmas" => Some("#dma-cells"),
        "iommus" => Some("#iommu-cells"),
        "interconnects" => Some("#interconnect-cells"),
        "interrupts-extended" => Some("#interrupt-cells"),
        "msi-parent" => Some("#msi-cells"),
        "thermal-sensors" => Some("#thermal-sensor-cells"),
        "sound-dai" => Some("#sound-dai-cells"),
        "mboxes" => Some("#mbox-cells"),
        "io-channels" => Some("#io-channel-cells"),
        "pwms" => Some("#pwm-cells"),
        "cooling-device" => Some("#cooling-cells"),
        "gpios" => Some("#gpio-cells"),
        _ if name.ends_with("-gpios") || name.ends_with("-gpio") => Some("#gpio-cells"),
        _ => None,
    };
    let single = matches!(
        name,
        "interrupt-parent"
            | "cpu"
            | "next-level-cache"
            | "phy-handle"
            | "syscon"
            | "regmap"
            | "remote-endpoint"
            | "zephyr,console"
            | "zephyr,shell-uart"
    ) || name.ends_with("-supply");
    if single && property.data.len() != 4 {
        return Err(invalid_reference(source, node, name));
    }
    let plain = single
        || matches!(
            name,
            "cpu-idle-states"
                | "operating-points-v2"
                | "required-opps"
                | "memory-region"
                | "nvmem-cells"
        )
        || name
            .strip_prefix("pinctrl-")
            .is_some_and(|suffix| suffix.parse::<u32>().is_ok());
    if provider_cells.is_none()
        && !plain
        && !matches!(name, "interrupt-map" | "msi-map" | "iommu-map")
    {
        return Ok(None);
    }
    if !property.data.len().is_multiple_of(4) {
        return Err(invalid_reference(source, node, name));
    }
    let cells = property.get_u32_iter().collect::<Vec<_>>();
    let mut offsets = Vec::new();
    let mut cursor = 0;
    while cursor < cells.len() {
        let prefix = match name {
            "interrupt-map" => cell_count(source, node, "#address-cells", Some(0))?
                .checked_add(cell_count(source, node, "#interrupt-cells", None)?)
                .ok_or_else(|| invalid_reference(source, node, name))?,
            "msi-map" | "iommu-map" => 1,
            _ => 0,
        };
        let offset = cursor
            .checked_add(prefix)
            .filter(|offset| *offset < cells.len())
            .ok_or_else(|| invalid_reference(source, node, name))?;
        // Empty GPIO/assigned-clock entries occupy one cell without a provider.
        if cells[offset] == 0
            && prefix == 0
            && (name == "next-level-cache"
                || name.starts_with("assigned-clock")
                || provider_cells == Some("#gpio-cells"))
        {
            // A zero next-level-cache is the firmware spelling for a CPU
            // without a cache provider. Preserve the property, but do not
            // treat the reserved zero as a source phandle to import.
            cursor += 1;
            continue;
        }
        let provider = *handles.get(&cells[offset]).ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                std::format!(
                    "{}:{name} references missing source phandle {:#x}",
                    source.path_of(node),
                    cells[offset]
                )
            )
        })?;
        let arguments = match name {
            "interrupt-map" => cell_count(source, provider, "#address-cells", Some(0))?
                .checked_add(cell_count(source, provider, "#interrupt-cells", None)?)
                .ok_or_else(|| invalid_reference(source, node, name))?,
            "msi-map" | "iommu-map" => 2,
            _ => match provider_cells {
                Some(cells) => {
                    cell_count(source, provider, cells, (name == "msi-parent").then_some(0))?
                }
                None => 0,
            },
        };
        cursor = offset
            .checked_add(1)
            .and_then(|end| end.checked_add(arguments))
            .filter(|end| *end <= cells.len())
            .ok_or_else(|| invalid_reference(source, node, name))?;
        offsets.push(offset);
    }
    Ok(Some(offsets))
}

fn cell_count(fdt: &Fdt, node: NodeId, name: &str, default: Option<usize>) -> AxVmResult<usize> {
    let property = fdt.node(node).and_then(|node| node.get_property(name));
    match property {
        Some(property) if property.data.len() == 4 => Ok(property.get_u32().unwrap() as usize),
        None if default.is_some() => Ok(default.unwrap()),
        _ => Err(invalid_reference(fdt, node, name)),
    }
}

fn invalid_reference(fdt: &Fdt, node: NodeId, name: &str) -> crate::AxVmError {
    ax_err_type!(
        InvalidData,
        std::format!(
            "invalid firmware reference layout at {}:{name}",
            fdt.path_of(node)
        )
    )
}
