//! FDT phandle-dependency closure for host-derived guest firmware.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use fdt_edit::Fdt;

use crate::machine::{MachinePlanError, MachinePlanResult};

type PhandleMap = BTreeMap<u32, PhandleProvider>;

struct PhandleProvider {
    path: String,
    cells: BTreeMap<String, u32>,
}

pub(super) fn resolve_dependencies(
    fdt: &Fdt,
    mut selected: BTreeSet<String>,
    protected: &BTreeMap<String, &'static str>,
) -> MachinePlanResult<BTreeSet<String>> {
    let providers = phandle_providers(fdt);
    let paths = fdt
        .iter_node_ids()
        .map(|node| (fdt.path_of(node), node))
        .collect::<BTreeMap<_, _>>();
    let mut pending = selected.iter().cloned().collect::<Vec<_>>();
    while let Some(path) = pending.pop() {
        let Some(node_id) = paths.get(&path).copied() else {
            continue;
        };
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        for property in node.properties() {
            if !is_phandle_property(property.name()) {
                continue;
            }
            for dependency in property_dependencies(property.name(), &property.data, &providers) {
                if let Some(classification) = protected.get(dependency.as_str()) {
                    return Err(MachinePlanError::InvalidFirmware {
                        detail: alloc::format!(
                            "FDT node '{path}' depends on {classification} node '{dependency}'"
                        ),
                    });
                }
                if selected.insert(dependency.clone()) {
                    pending.push(dependency);
                }
            }
        }
    }
    Ok(selected)
}

fn phandle_providers(fdt: &Fdt) -> PhandleMap {
    let mut providers = BTreeMap::new();
    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let mut phandle = None;
        let mut cells = BTreeMap::new();
        for property in node.properties() {
            match property.name() {
                "phandle" | "linux,phandle" => phandle = property.get_u32(),
                name if name.starts_with('#') && name.ends_with("-cells") => {
                    if let Some(value) = property.get_u32() {
                        cells.insert(name.into(), value);
                    }
                }
                _ => {}
            }
        }
        if let Some(phandle) = phandle {
            providers.insert(
                phandle,
                PhandleProvider {
                    path: fdt.path_of(node_id),
                    cells,
                },
            );
        }
    }
    providers
}

fn property_dependencies(name: &str, bytes: &[u8], providers: &PhandleMap) -> Vec<String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    let cells = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|cell| u32::from_be_bytes(*cell))
        .collect::<Vec<_>>();
    let mut dependencies = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let Some(provider) = providers.get(&cells[index]) else {
            index += 1;
            continue;
        };
        dependencies.push(provider.path.clone());
        let arguments = argument_cell_count(name, &provider.cells);
        index = index.saturating_add(arguments + 1);
    }
    dependencies
}

fn argument_cell_count(name: &str, cells: &BTreeMap<String, u32>) -> usize {
    let cell_property = match name {
        "interrupt-parent" | "phy-handle" | "msi-parent" | "memory-region" | "syscon"
        | "regmap" | "nvmem-cells" => return 0,
        "clocks" | "assigned-clocks" => "#clock-cells",
        "resets" => "#reset-cells",
        "power-domains" => "#power-domain-cells",
        "phys" => "#phy-cells",
        "interrupts-extended" => "#interrupt-cells",
        "gpios" => "#gpio-cells",
        name if name.ends_with("-gpios") || name.ends_with("-gpio") => "#gpio-cells",
        "dmas" => "#dma-cells",
        "thermal-sensors" => "#thermal-sensor-cells",
        "sound-dai" => "#sound-dai-cells",
        "mboxes" => "#mbox-cells",
        "pwms" => "#pwm-cells",
        "iommus" => "#iommu-cells",
        _ => return 0,
    };
    cells.get(cell_property).copied().unwrap_or(0) as usize
}

fn is_phandle_property(name: &str) -> bool {
    matches!(
        name,
        "clocks"
            | "power-domains"
            | "phys"
            | "resets"
            | "dmas"
            | "thermal-sensors"
            | "mboxes"
            | "assigned-clocks"
            | "interrupt-parent"
            | "interrupts-extended"
            | "phy-handle"
            | "msi-parent"
            | "memory-region"
            | "syscon"
            | "regmap"
            | "iommus"
            | "interconnects"
            | "nvmem-cells"
            | "sound-dai"
            | "pinctrl-0"
            | "pinctrl-1"
            | "pinctrl-2"
            | "pinctrl-3"
            | "pinctrl-4"
            | "gpios"
            | "pwms"
    ) || name.ends_with("-supply")
        || name.ends_with("-gpios")
        || name.ends_with("-gpio")
}
