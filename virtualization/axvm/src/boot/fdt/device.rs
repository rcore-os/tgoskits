// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Device passthrough and dependency analysis for FDT processing.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    vec::Vec,
};

use fdt_edit::{Fdt, NodeId};

use crate::config::AxVMConfig;

type NodeCache = BTreeMap<String, Vec<NodeId>>;
type PhandleMap = BTreeMap<u32, (String, BTreeMap<String, u32>)>;

/// Return all passthrough device paths, including descendants and phandle dependencies.
pub fn find_all_passthrough_devices(vm_cfg: &mut AxVMConfig, fdt: &Fdt) -> Vec<String> {
    let initial_device_count = vm_cfg.pass_through_devices().len();
    let node_cache = build_optimized_node_cache(fdt);
    let initial_device_names: Vec<String> = vm_cfg
        .pass_through_devices()
        .iter()
        .map(|dev| dev.name.clone())
        .collect();
    let mut configured_device_names: BTreeSet<String> =
        initial_device_names.iter().cloned().collect();
    let mut additional_device_names = Vec::new();

    for device_name in &initial_device_names {
        let descendant_paths = get_descendant_nodes_by_path(&node_cache, device_name);
        trace!(
            "Found {} descendant paths for {}",
            descendant_paths.len(),
            device_name
        );

        for descendant_path in descendant_paths {
            if configured_device_names.insert(descendant_path.clone()) {
                trace!("Found descendant device: {descendant_path}");
                additional_device_names.push(descendant_path);
            }
        }
    }

    let mut dependency_device_names = Vec::new();
    let mut devices_to_process: Vec<String> = configured_device_names.iter().cloned().collect();
    let mut processed_devices: BTreeSet<String> = BTreeSet::new();
    let phandle_map = build_phandle_map(fdt);

    while let Some(device_node_path) = devices_to_process.pop() {
        if !processed_devices.insert(device_node_path.clone()) {
            continue;
        }

        let dependencies =
            find_device_dependencies(fdt, &device_node_path, &phandle_map, &node_cache);
        for dep_node_name in dependencies {
            if configured_device_names.insert(dep_node_name.clone()) {
                trace!("Found new dependency device: {dep_node_name}");
                dependency_device_names.push(dep_node_name.clone());
                devices_to_process.push(dep_node_name);
            }
        }
    }

    let excluded_device_path: Vec<String> = vm_cfg
        .excluded_devices()
        .iter()
        .flatten()
        .cloned()
        .collect();
    let mut all_excluded_devices = excluded_device_path.clone();
    let mut processed_excluded: BTreeSet<String> = excluded_device_path.iter().cloned().collect();

    for device_path in &excluded_device_path {
        for descendant_path in get_descendant_nodes_by_path(&node_cache, device_path) {
            if processed_excluded.insert(descendant_path.clone()) {
                all_excluded_devices.push(descendant_path);
            }
        }
    }
    info!("Found excluded devices: {all_excluded_devices:?}");

    let mut all_device_names = initial_device_names;
    all_device_names.extend(additional_device_names);
    all_device_names.extend(dependency_device_names);

    if !all_excluded_devices.is_empty() {
        let excluded_set: BTreeSet<String> = all_excluded_devices.into_iter().collect();
        all_device_names.retain(|device_name| {
            let should_keep = !excluded_set.contains(device_name);
            if !should_keep {
                info!("Excluding device: {device_name}");
            }
            should_keep
        });
    }

    all_device_names.retain(|device_name| device_name != "/");

    debug!(
        "Passthrough devices analysis completed. Total devices: {} (added: {})",
        all_device_names.len(),
        all_device_names.len().saturating_sub(initial_device_count)
    );
    all_device_names
}

pub fn build_optimized_node_cache(fdt: &Fdt) -> NodeCache {
    let mut node_cache = BTreeMap::new();

    for node_id in fdt.iter_node_ids() {
        let node_path = fdt.path_of(node_id);
        node_cache
            .entry(node_path)
            .or_insert_with(Vec::new)
            .push(node_id);
    }

    debug!(
        "Built simplified node cache with {} unique device paths",
        node_cache.len()
    );
    node_cache
}

fn build_phandle_map(fdt: &Fdt) -> PhandleMap {
    let mut phandle_map = BTreeMap::new();

    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let node_path = fdt.path_of(node_id);
        let mut phandle = None;
        let mut cells_map = BTreeMap::new();

        for prop in node.properties() {
            match prop.name() {
                "phandle" | "linux,phandle" => phandle = prop.get_u32(),
                "#address-cells"
                | "#size-cells"
                | "#clock-cells"
                | "#reset-cells"
                | "#gpio-cells"
                | "#interrupt-cells"
                | "#power-domain-cells"
                | "#thermal-sensor-cells"
                | "#phy-cells"
                | "#dma-cells"
                | "#sound-dai-cells"
                | "#mbox-cells"
                | "#pwm-cells"
                | "#iommu-cells" => {
                    if let Some(value) = prop.get_u32() {
                        cells_map.insert(prop.name().to_string(), value);
                    }
                }
                _ => {}
            }
        }

        if let Some(ph) = phandle {
            phandle_map.insert(ph, (node_path, cells_map));
        }
    }
    phandle_map
}

fn parse_phandle_property_with_cells(
    prop_data: &[u8],
    prop_name: &str,
    phandle_map: &PhandleMap,
) -> Vec<(u32, Vec<u32>)> {
    let mut results = Vec::new();

    if prop_data.is_empty() || !prop_data.len().is_multiple_of(4) {
        return results;
    }

    let u32_values: Vec<u32> = prop_data
        .chunks(4)
        .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    let mut i = 0;
    while i < u32_values.len() {
        let potential_phandle = u32_values[i];
        if let Some((device_name, cells_info)) = phandle_map.get(&potential_phandle) {
            let cells_count = get_cells_count_for_property(prop_name, cells_info);
            if i + cells_count < u32_values.len() {
                let specifiers = u32_values[i + 1..=i + cells_count].to_vec();
                debug!(
                    "Parsed {prop_name} phandle reference: phandle={potential_phandle:#x}, \
                     device={device_name}, specifiers={specifiers:?}"
                );
                results.push((potential_phandle, specifiers));
                i += cells_count + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }

    results
}

fn get_cells_count_for_property(prop_name: &str, cells_info: &BTreeMap<String, u32>) -> usize {
    let cells_property = match prop_name {
        "clocks" | "assigned-clocks" => "#clock-cells",
        "resets" => "#reset-cells",
        "power-domains" => "#power-domain-cells",
        "phys" => "#phy-cells",
        "interrupts" | "interrupts-extended" => "#interrupt-cells",
        "gpios" => "#gpio-cells",
        _ if prop_name.ends_with("-gpios") || prop_name.ends_with("-gpio") => "#gpio-cells",
        "dmas" => "#dma-cells",
        "thermal-sensors" => "#thermal-sensor-cells",
        "sound-dai" => "#sound-dai-cells",
        "mboxes" => "#mbox-cells",
        "pwms" => "#pwm-cells",
        _ => return 0,
    };

    cells_info.get(cells_property).copied().unwrap_or(0) as usize
}

fn parse_phandle_property(
    prop_data: &[u8],
    prop_name: &str,
    phandle_map: &PhandleMap,
) -> Vec<String> {
    parse_phandle_property_with_cells(prop_data, prop_name, phandle_map)
        .into_iter()
        .filter_map(|(phandle, _)| phandle_map.get(&phandle).map(|(path, _)| path.clone()))
        .collect()
}

struct DevicePropertyClassifier;

impl DevicePropertyClassifier {
    const PHANDLE_PROPERTIES: &'static [&'static str] = &[
        "clocks",
        "power-domains",
        "phys",
        "resets",
        "dmas",
        "thermal-sensors",
        "mboxes",
        "assigned-clocks",
        "interrupt-parent",
        "phy-handle",
        "msi-parent",
        "memory-region",
        "syscon",
        "regmap",
        "iommus",
        "interconnects",
        "nvmem-cells",
        "sound-dai",
        "pinctrl-0",
        "pinctrl-1",
        "pinctrl-2",
        "pinctrl-3",
        "pinctrl-4",
    ];

    fn is_phandle_property(prop_name: &str) -> bool {
        Self::PHANDLE_PROPERTIES.contains(&prop_name)
            || prop_name.ends_with("-supply")
            || prop_name == "gpios"
            || prop_name.ends_with("-gpios")
            || prop_name.ends_with("-gpio")
            || (prop_name.contains("cells") && !prop_name.starts_with('#') && prop_name.len() >= 4)
    }
}

fn find_device_dependencies(
    fdt: &Fdt,
    device_node_path: &str,
    phandle_map: &PhandleMap,
    node_cache: &NodeCache,
) -> Vec<String> {
    let mut dependencies = Vec::new();

    if let Some(nodes) = node_cache.get(device_node_path) {
        for node_id in nodes {
            let Some(node) = fdt.node(*node_id) else {
                continue;
            };
            for prop in node.properties() {
                if DevicePropertyClassifier::is_phandle_property(prop.name()) {
                    dependencies.extend(parse_phandle_property(
                        &prop.data,
                        prop.name(),
                        phandle_map,
                    ));
                }
            }
        }
    }

    dependencies
}

fn get_descendant_nodes_by_path(node_cache: &NodeCache, parent_path: &str) -> Vec<String> {
    let search_prefix = if parent_path == "/" {
        "/".to_string()
    } else {
        parent_path.to_string() + "/"
    };

    node_cache
        .keys()
        .filter(|path| path.starts_with(&search_prefix) && path.len() > search_prefix.len())
        .cloned()
        .collect()
}
