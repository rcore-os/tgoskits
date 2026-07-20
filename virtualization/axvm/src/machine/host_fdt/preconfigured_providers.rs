//! Firmware substitutions for provider resources pinned by the host.

use alloc::{format, string::String, vec, vec::Vec};

use fdt_edit::{Fdt, NodeId, Property};

use super::{dependencies::FdtDependencyIndex, fixed_clock::add_fixed_clock};
use crate::machine::{
    HostProviderReferenceKind, MachinePlanError, MachinePlanResult, PreconfiguredHostClock,
    PreconfiguredHostDeviceResources, PreconfiguredHostReset, VmMachinePlan,
};

pub(super) fn materialize_preconfigured_provider_resources(
    source: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    for resources in plan.preconfigured_host_devices() {
        materialize_device_resources(source, resources)?;
    }
    Ok(())
}

fn materialize_device_resources(
    source: &mut Fdt,
    resources: &PreconfiguredHostDeviceResources,
) -> MachinePlanResult<()> {
    let path = resources.device().as_str();
    let node_id = source
        .get_by_path_id(path)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("preconfigured host device '{path}' is absent from the source FDT"),
        })?;
    let dependencies = FdtDependencyIndex::new(source);
    let node = source
        .node(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("preconfigured host device '{path}' cannot be inspected"),
        })?;
    let references = dependencies.dependencies(node);
    let clock_cells = materialize_clock_cells(path, source, &references, resources.clocks())?;
    if !resources.clock_configurations().is_empty() {
        validate_clock_configuration_references(
            path,
            &references,
            resources.clock_configurations(),
        )?;
    }
    let reset_rewrite = plan_reset_rewrite(path, source, node_id, &references, resources.resets())?;
    let node = source
        .node_mut(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("preconfigured host device '{path}' cannot be updated"),
        })?;
    if let Some(clock_cells) = clock_cells {
        let mut clocks = Property::new("clocks", Vec::new());
        clocks.set_u32_ls(&clock_cells);
        node.set_property(clocks);
    }
    if !resources.clock_configurations().is_empty() {
        for property in [
            "assigned-clocks",
            "assigned-clock-parents",
            "assigned-clock-rates",
        ] {
            node.remove_property(property);
        }
    }
    match reset_rewrite {
        ResetRewrite::Unchanged => {}
        ResetRewrite::Remove => {
            node.remove_property("resets");
            node.remove_property("reset-names");
        }
        ResetRewrite::Retain { cells, names } => {
            let mut resets = Property::new("resets", Vec::new());
            resets.set_u32_ls(&cells);
            node.set_property(resets);
            if let Some(names) = names {
                let names = names.iter().map(String::as_str).collect::<Vec<_>>();
                let mut property = Property::new("reset-names", Vec::new());
                property.set_string_ls(&names);
                node.set_property(property);
            }
        }
    }
    Ok(())
}

fn materialize_clock_cells(
    path: &str,
    source: &mut Fdt,
    references: &[super::dependencies::FdtNodeDependency],
    planned: &[PreconfiguredHostClock],
) -> MachinePlanResult<Option<Vec<u32>>> {
    if planned.is_empty() {
        return Ok(None);
    }
    let source_clocks = references
        .iter()
        .filter(|reference| reference.reference().kind() == HostProviderReferenceKind::Clock)
        .collect::<Vec<_>>();
    let mut consumed = vec![false; planned.len()];
    let mut cells = Vec::new();
    for source_clock in source_clocks {
        if let Some(index) = matching_clock_index(source_clock, planned, &consumed) {
            consumed[index] = true;
            cells.push(add_fixed_clock(source, planned[index].rate_hz().get())?);
        } else {
            cells.push(provider_phandle(source, source_clock.provider())?);
            cells.extend_from_slice(source_clock.reference().specifier());
        }
    }
    if consumed.iter().all(|consumed| *consumed) {
        return Ok(Some(cells));
    }
    Err(MachinePlanError::InvalidFirmware {
        detail: format!(
            "preconfigured clocks for host device '{path}' no longer match the source FDT"
        ),
    })
}

fn matching_clock_index(
    source: &super::dependencies::FdtNodeDependency,
    planned: &[PreconfiguredHostClock],
    consumed: &[bool],
) -> Option<usize> {
    planned.iter().enumerate().position(|(index, planned)| {
        !consumed[index]
            && source.provider() == planned.provider().as_str()
            && source.reference().specifier() == planned.specifier()
    })
}

enum ResetRewrite {
    Unchanged,
    Remove,
    Retain {
        cells: Vec<u32>,
        names: Option<Vec<String>>,
    },
}

fn plan_reset_rewrite(
    path: &str,
    source: &Fdt,
    node_id: NodeId,
    references: &[super::dependencies::FdtNodeDependency],
    planned: &[PreconfiguredHostReset],
) -> MachinePlanResult<ResetRewrite> {
    if planned.is_empty() {
        return Ok(ResetRewrite::Unchanged);
    }
    let source_resets = references
        .iter()
        .filter(|reference| reference.reference().kind() == HostProviderReferenceKind::Reset)
        .collect::<Vec<_>>();
    let mut consumed = vec![false; planned.len()];
    let mut retained = Vec::new();
    let mut retained_indices = Vec::new();
    for (source_index, source_reset) in source_resets.iter().enumerate() {
        if let Some(index) = matching_reset_index(source_reset, planned, &consumed) {
            consumed[index] = true;
        } else {
            retained.push(*source_reset);
            retained_indices.push(source_index);
        }
    }
    if !consumed.iter().all(|consumed| *consumed) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "preconfigured resets for host device '{path}' no longer match the source FDT"
            ),
        });
    }
    if retained.is_empty() {
        return Ok(ResetRewrite::Remove);
    }

    let mut cells = Vec::new();
    for reset in retained {
        cells.push(provider_phandle(source, reset.provider())?);
        cells.extend_from_slice(reset.reference().specifier());
    }
    let names = retained_reset_names(
        path,
        source,
        node_id,
        source_resets.len(),
        &retained_indices,
    )?;
    Ok(ResetRewrite::Retain { cells, names })
}

fn matching_reset_index(
    source: &super::dependencies::FdtNodeDependency,
    planned: &[PreconfiguredHostReset],
    consumed: &[bool],
) -> Option<usize> {
    planned.iter().enumerate().position(|(index, planned)| {
        !consumed[index]
            && source.provider() == planned.provider().as_str()
            && source.reference().specifier() == planned.specifier()
    })
}

fn retained_reset_names(
    path: &str,
    source: &Fdt,
    node_id: NodeId,
    reset_count: usize,
    retained_indices: &[usize],
) -> MachinePlanResult<Option<Vec<String>>> {
    let Some(property) = source
        .node(node_id)
        .and_then(|node| node.get_property("reset-names"))
    else {
        return Ok(None);
    };
    let names = property.as_str_iter().map(String::from).collect::<Vec<_>>();
    if names.len() != reset_count {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "reset-names for host device '{path}' does not match its reset references"
            ),
        });
    }
    Ok(Some(
        retained_indices
            .iter()
            .map(|index| names[*index].clone())
            .collect(),
    ))
}

fn provider_phandle(source: &Fdt, path: &str) -> MachinePlanResult<u32> {
    source
        .get_by_path(path)
        .and_then(|provider| {
            let provider = provider.as_node();
            provider
                .get_property("phandle")
                .or_else(|| provider.get_property("linux,phandle"))
                .and_then(Property::get_u32)
        })
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("host provider '{path}' has no usable phandle"),
        })
}

fn validate_clock_configuration_references(
    path: &str,
    references: &[super::dependencies::FdtNodeDependency],
    planned: &[PreconfiguredHostClock],
) -> MachinePlanResult<()> {
    let source = references
        .iter()
        .filter(|reference| {
            reference.reference().kind() == HostProviderReferenceKind::ClockConfiguration
        })
        .collect::<Vec<_>>();
    let valid = source.len() == planned.len()
        && source.iter().zip(planned).all(|(source, planned)| {
            source.provider() == planned.provider().as_str()
                && source.reference().specifier() == planned.specifier()
        });
    if valid {
        return Ok(());
    }
    Err(MachinePlanError::InvalidFirmware {
        detail: format!(
            "preconfigured clock settings for host device '{path}' no longer match the source FDT"
        ),
    })
}
