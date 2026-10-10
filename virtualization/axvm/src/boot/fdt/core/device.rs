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

use std::{collections::BTreeSet, string::String, vec::Vec};

use fdt_edit::{Fdt, NodeId};

use super::references::{phandle_index, reference_offsets};
use crate::{AxVmResult, ax_err_type, config::AxVMConfig};

/// Selects a node and its descendants, never a similarly prefixed sibling.
pub(crate) fn selector_includes_path(selector: &str, node_path: &str) -> bool {
    selector == node_path
        || node_path
            .strip_prefix(selector)
            .is_some_and(|suffix| selector == "/" || suffix.starts_with('/'))
}

/// Returns assigned device paths and their descriptive firmware dependencies.
pub fn find_all_passthrough_devices(vm_cfg: &AxVMConfig, fdt: &Fdt) -> AxVmResult<Vec<String>> {
    let selected = vm_cfg
        .pass_through_devices()
        .iter()
        .map(|dev| dev.name.clone())
        .collect::<Vec<_>>();
    let excluded = vm_cfg
        .excluded_devices()
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    find_all_passthrough_devices_from_paths(&selected, &excluded, fdt)
}

/// Resolves references without granting unselected providers MMIO or IRQ access.
pub(crate) fn find_all_passthrough_devices_from_paths(
    selectors: &[String],
    excluded: &[String],
    fdt: &Fdt,
) -> AxVmResult<Vec<String>> {
    let handles = phandle_index(fdt)?;
    let excluded_path = |path: &str| excluded.iter().any(|p| selector_includes_path(p, path));
    let assigned = |path: &str| selectors.iter().any(|p| selector_includes_path(p, path));
    let mut pending = fdt
        .iter_node_ids()
        .filter(|&id| {
            let path = fdt.path_of(id);
            assigned(&path) && !excluded_path(&path) && node_enabled(fdt, id)
        })
        .collect::<Vec<_>>();
    let mut selected = pending.iter().copied().collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let node = fdt.node(id).unwrap();
        for property in node.properties() {
            let Some(offsets) = reference_offsets(fdt, &handles, id, property)? else {
                continue;
            };
            let cells = property.get_u32_iter().collect::<Vec<_>>();
            for offset in offsets {
                let provider = handles[&cells[offset]];
                for dependency in provider_nodes(fdt, provider, property.name()) {
                    let path = fdt.path_of(dependency);
                    // Explicit exclusions can omit optional descriptive children.
                    if dependency != provider && excluded_path(&path) {
                        continue;
                    }
                    // Machine interrupt providers are installed by their device models.
                    if super::interrupt::is_machine_interrupt_provider(
                        fdt.node(dependency).unwrap(),
                    ) {
                        continue;
                    }
                    if excluded_path(&path) {
                        return Err(ax_err_type!(
                            InvalidInput,
                            std::format!(
                                "{}:{} depends on disabled provider {path}",
                                fdt.path_of(id),
                                property.name()
                            )
                        ));
                    }
                    selected.insert(dependency);
                    // Firmware graphs may legally point at an inactive endpoint.
                    // Keep its identity without assigning its resources or dependencies.
                    if !node_enabled(fdt, dependency) {
                        continue;
                    }
                    let mut ancestor = Some(dependency);
                    while let Some(current) = ancestor {
                        let path = fdt.path_of(current);
                        let node = fdt.node(current).unwrap();
                        let owns_resources = owns_device_resources(node);
                        if owns_resources
                            && !assigned(&path)
                            && !selector_includes_path("/reserved-memory", &path)
                            && !selector_includes_path("/cpus", &path)
                        {
                            return Err(ax_err_type!(
                                InvalidInput,
                                std::format!(
                                    "{}:{} requires explicit passthrough assignment of {path}",
                                    fdt.path_of(id),
                                    property.name()
                                )
                            ));
                        }
                        if current != fdt.root_id() {
                            pending.push(current);
                        }
                        ancestor = fdt.parent_of(current);
                    }
                }
            }
        }
    }
    let mut paths = selected
        .into_iter()
        .map(|id| fdt.path_of(id))
        .filter(|path| {
            path != "/"
                && !excluded_path(path)
                && (!owns_device_resources(fdt.get_by_path(path).unwrap().as_node())
                    || !excluded.iter().any(|p| {
                        fdt.get_by_path_id(p).is_some() && selector_includes_path(path, p)
                    }))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

// These children describe their provider; other children may be independent devices.
fn provider_nodes(fdt: &Fdt, provider: NodeId, reference: &str) -> Vec<NodeId> {
    let mut nodes = std::vec![provider];
    if !node_enabled(fdt, provider) {
        return nodes;
    }
    nodes.extend(
        fdt.node(provider)
            .unwrap()
            .children()
            .iter()
            .copied()
            .filter(|&id| {
                let name = fdt.node(id).unwrap().name();
                if reference == "operating-points-v2" {
                    // An OPP table's children are its entries; names vary by binding.
                    true
                } else if reference.ends_with("-supply") {
                    matches!(
                        name,
                        "regulator-state-standby" | "regulator-state-mem" | "regulator-state-disk"
                    )
                } else {
                    false
                }
            }),
    );
    nodes
}

fn owns_device_resources(node: &fdt_edit::Node) -> bool {
    ["reg", "ranges", "interrupts", "interrupts-extended"]
        .iter()
        .any(|name| node.get_property(name).is_some_and(|p| !p.data.is_empty()))
}

/// A disabled ancestor also disables its entire device subtree.
pub(super) fn node_enabled(fdt: &Fdt, mut id: NodeId) -> bool {
    loop {
        if let Some(status) = fdt.node(id).and_then(|n| n.get_property("status"))
            && !matches!(status.data.as_slice(), b"okay\0" | b"ok\0")
        {
            return false;
        }
        match fdt.parent_of(id) {
            Some(parent) => id = parent,
            None => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use fdt_edit::{Node, Property};
    use fdt_raw::RegInfo;

    use super::*;

    fn cells(name: &str, values: &[u32]) -> Property {
        let mut property = Property::new(name, Vec::new());
        property.set_u32_ls(values);
        property
    }

    fn add_cpu_template(fdt: &mut Fdt) {
        let cpus = fdt.add_node(fdt.root_id(), Node::new("cpus"));
        let cpu = fdt.add_node(cpus, Node::new("cpu@0"));
        fdt.view_typed_mut(cpu)
            .unwrap()
            .set_regs(&[RegInfo::new(0, None)]);
    }

    #[test]
    fn dependency_discovery_keeps_descriptive_children_without_assigning_sibling_devices() {
        let mut fdt = Fdt::new();
        add_cpu_template(&mut fdt);
        let root = fdt.root_id();
        let device = fdt.add_node(root, Node::new("device"));
        fdt.node_mut(device)
            .unwrap()
            .set_property(cells("vdd-supply", &[1]));
        fdt.node_mut(device)
            .unwrap()
            .set_property(cells("operating-points-v2", &[2]));
        let regulator = fdt.add_node(root, Node::new("regulator"));
        fdt.node_mut(regulator)
            .unwrap()
            .set_property(cells("phandle", &[1]));
        let state = fdt.add_node(regulator, Node::new("regulator-state-mem"));
        fdt.node_mut(state)
            .unwrap()
            .set_property(cells("regulator-suspend-microvolt", &[900_000]));
        let table = fdt.add_node(root, Node::new("opp-table"));
        fdt.node_mut(table)
            .unwrap()
            .set_property(cells("phandle", &[2]));
        let opp = fdt.add_node(table, Node::new("opp100000000"));
        fdt.node_mut(opp)
            .unwrap()
            .set_property(cells("opp-hz", &[0, 100_000_000]));
        fdt.node_mut(opp)
            .unwrap()
            .set_property(cells("required-opps", &[3]));
        let required_table = fdt.add_node(root, Node::new("required-table"));
        let required_opp = fdt.add_node(required_table, Node::new("opp-1"));
        fdt.node_mut(required_opp)
            .unwrap()
            .set_property(cells("phandle", &[3]));
        let unrelated = fdt.add_node(regulator, Node::new("device@1000"));
        fdt.node_mut(unrelated)
            .unwrap()
            .set_property(cells("reg", &[0x1000, 0x100]));
        let selectors = ["/device".into()];
        let selected = find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).unwrap();
        let mut cfg = axvmconfig::GuestConfig::default();
        cfg.base.phys_cpu_ids = Some(std::vec![0]);
        let bytes = super::super::create::create_guest_fdt(&fdt, &selected, &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        let state = guest.get_by_path("/regulator/regulator-state-mem").unwrap();
        assert_eq!(
            state
                .as_node()
                .get_property("regulator-suspend-microvolt")
                .unwrap()
                .get_u32(),
            Some(900_000)
        );
        let point = guest.get_by_path("/opp-table/opp100000000").unwrap();
        assert_eq!(
            point
                .as_node()
                .get_property("opp-hz")
                .unwrap()
                .get_u32_iter()
                .collect::<Vec<_>>(),
            [0, 100_000_000]
        );
        assert!(guest.get_by_path_id("/required-table/opp-1").is_some());
        assert!(guest.get_by_path_id("/regulator/device@1000").is_none());
        assert!(
            find_all_passthrough_devices_from_paths(
                &selectors,
                &["/required-table/opp-1".into()],
                &fdt
            )
            .is_err()
        );
        let excluded = ["/regulator/regulator-state-mem".into()];
        let selected =
            find_all_passthrough_devices_from_paths(&selectors, &excluded, &fdt).unwrap();
        let bytes =
            super::super::create::create_guest_fdt(&fdt, &selected, &cfg, &excluded).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        assert!(guest.get_by_path_id("/regulator").is_some());
        assert!(guest.get_by_path_id(&excluded[0]).is_none());
        fdt.node_mut(opp)
            .unwrap()
            .set_property(cells("interrupts", &[1]));
        assert!(find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).is_err());
        fdt.node_mut(opp).unwrap().remove_property("interrupts");
        fdt.node_mut(opp)
            .unwrap()
            .set_property(cells("reg", &[0x2000, 0x100]));
        assert!(find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).is_err());
    }

    #[test]
    fn dependency_discovery_does_not_interpret_provider_arguments_as_devices() {
        let mut fdt = Fdt::new();
        add_cpu_template(&mut fdt);
        let root = fdt.root_id();
        let device = fdt.add_node(root, Node::new("device"));
        fdt.node_mut(device)
            .unwrap()
            .set_property(cells("iommus", &[1, 2]));
        let fabric = fdt.add_node(root, Node::new("fabric"));
        let provider = fdt.add_node(fabric, Node::new("iommu"));
        fdt.node_mut(provider)
            .unwrap()
            .set_property(cells("phandle", &[1]));
        fdt.node_mut(provider)
            .unwrap()
            .set_property(cells("#iommu-cells", &[1]));
        let unrelated = fdt.add_node(fabric, Node::new("unrelated"));
        fdt.node_mut(unrelated)
            .unwrap()
            .set_property(cells("phandle", &[2]));
        let selected =
            find_all_passthrough_devices_from_paths(&["/device".into()], &[], &fdt).unwrap();
        assert!(selected.contains(&"/fabric/iommu".into()));
        assert!(!selected.contains(&"/fabric".into()));
        let mut cfg = axvmconfig::GuestConfig::default();
        cfg.base.phys_cpu_ids = Some(std::vec![0]);
        let bytes = super::super::create::create_guest_fdt(&fdt, &selected, &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        assert!(guest.get_by_path_id("/fabric/iommu").is_some());
        assert!(guest.get_by_path_id("/fabric/unrelated").is_none());
        assert!(!selected.contains(&"/fabric/unrelated".into()));
        let selectors = ["/device".into()];
        assert!(
            find_all_passthrough_devices_from_paths(&selectors, &["/fabric/iommu".into()], &fdt)
                .is_err()
        );
        fdt.node_mut(provider)
            .unwrap()
            .set_property(cells("reg", &[0, 0x1000, 0x100]));
        assert!(find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).is_err());
        let selected = find_all_passthrough_devices_from_paths(
            &["/device".into(), "/fabric/iommu".into()],
            &[],
            &fdt,
        )
        .unwrap();
        assert!(!selected.contains(&"/fabric/unrelated".into()));
        fdt.node_mut(provider).unwrap().remove_property("reg");
        for malformed in [&[1][..], &[99, 2][..]] {
            fdt.node_mut(device)
                .unwrap()
                .set_property(cells("iommus", malformed));
            assert!(find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).is_err());
        }
        fdt.node_mut(device).unwrap().remove_property("iommus");
        fdt.node_mut(device)
            .unwrap()
            .set_property(cells("interrupt-parent", &[1, 2]));
        assert!(find_all_passthrough_devices_from_paths(&selectors, &[], &fdt).is_err());
    }
}
