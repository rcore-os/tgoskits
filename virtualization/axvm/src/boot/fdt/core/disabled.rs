//! Applies explicit firmware exclusions before publishing either DTB source.

use std::{collections::BTreeSet, string::String, vec::Vec};

use axvmconfig::GuestConfig;

use super::{
    device::{node_enabled, selector_includes_path},
    references::{phandle_index, reference_offsets},
    tree::FdtTree,
};
use crate::{AxVmResult, ax_err_type};

pub(super) fn apply(tree: &mut FdtTree, config: &GuestConfig) -> AxVmResult {
    if config.devices.disabled.is_empty() {
        return Ok(());
    }
    let fdt = tree.inner();
    let excluded = |path: &str| {
        config
            .devices
            .disabled
            .iter()
            .any(|d| selector_includes_path(&d.path, path))
    };
    let mut paths = Vec::new();
    let mut removed = BTreeSet::new();
    for id in fdt.iter_node_ids() {
        let path = fdt.path_of(id);
        if !excluded(&path) {
            continue;
        }
        let node = fdt.node(id).unwrap();
        if path == "/"
            || path == "/cpus"
            || path.starts_with("/cpus/")
            || super::interrupt::is_machine_interrupt_provider(node)
            || super::timer::is_architectural_timer_node(node)
        {
            return Err(ax_err_type!(
                InvalidInput,
                std::format!("cannot disable required machine firmware node {path}")
            ));
        }
        removed.insert(id);
        paths.push(path);
    }
    let handles = phandle_index(fdt)?;
    for id in fdt.iter_node_ids() {
        if removed.contains(&id) || !node_enabled(fdt, id) {
            continue;
        }
        for property in fdt.node(id).unwrap().properties() {
            let Some(offsets) = reference_offsets(fdt, &handles, id, property)? else {
                continue;
            };
            let cells = property.get_u32_iter().collect::<Vec<_>>();
            for offset in offsets {
                if removed.contains(&handles[&cells[offset]]) {
                    return Err(ax_err_type!(
                        InvalidInput,
                        std::format!(
                            "{}:{} references explicitly disabled provider {}",
                            fdt.path_of(id),
                            property.name(),
                            fdt.path_of(handles[&cells[offset]])
                        )
                    ));
                }
            }
        }
    }
    let stale_aliases = fdt
        .get_by_path("/aliases")
        .map(|node| {
            node.as_node()
                .properties()
                .iter()
                .filter(|property| property.as_str().is_some_and(&excluded))
                .map(|p| String::from(p.name()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // String paths in chosen may use aliases or include console options.
    let stale_chosen = fdt
        .get_by_path("/chosen")
        .map(|node| {
            node.as_node()
                .properties()
                .iter()
                .filter(|property| {
                    matches!(
                        property.name(),
                        "stdout-path" | "stdin-path" | "linux,stdout-path"
                    ) && property.as_str().is_some_and(|value| {
                        let path = value.split(':').next().unwrap_or(value);
                        excluded(path) || stale_aliases.iter().any(|alias| alias == path)
                    })
                })
                .map(|p| String::from(p.name()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (path, properties) in [("/aliases", stale_aliases), ("/chosen", stale_chosen)] {
        if let Some(id) = tree.inner().get_by_path_id(path) {
            for property in properties {
                tree.inner_mut()
                    .node_mut(id)
                    .unwrap()
                    .remove_property(&property);
            }
        }
    }
    paths.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    for path in paths {
        tree.inner_mut().remove_by_path(&path);
    }
    Ok(())
}

/// Device-model installation must not resurrect an explicitly disabled path.
pub(super) fn validate_runtime(fdt: &fdt_edit::Fdt, config: &GuestConfig) -> AxVmResult {
    for device in &config.devices.disabled {
        if fdt
            .iter_node_ids()
            .any(|id| selector_includes_path(&device.path, &fdt.path_of(id)))
        {
            return Err(ax_err_type!(
                InvalidInput,
                std::format!(
                    "machine device installation conflicts with disabled path {}",
                    device.path
                )
            ));
        }
    }
    Ok(())
}
