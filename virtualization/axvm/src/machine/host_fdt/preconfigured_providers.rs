//! Firmware substitutions for static and VM-mediated provider resources.

use alloc::{collections::BTreeSet, format, string::String, vec, vec::Vec};

use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

use super::{
    dependencies::FdtDependencyIndex,
    fixed_clock::{add_fixed_clock, next_phandle},
};
use crate::machine::{
    ArmScmiMediationPlan, HostProviderReferenceKind, MachinePlanError, MachinePlanResult,
    PreconfiguredHostClock, PreconfiguredHostDeviceResources, PreconfiguredHostReset,
    VmMachinePlan,
};

const SCMI_PATH: &str = "/firmware/scmi-65535";

#[derive(Clone, Copy)]
struct MediationNodes {
    clock_phandle: Option<u32>,
    reset_phandle: Option<u32>,
}

fn add_scmi_nodes(
    source: &mut Fdt,
    mediation: &ArmScmiMediationPlan,
) -> MachinePlanResult<MediationNodes> {
    if source.get_by_path_id(SCMI_PATH).is_some() {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!("reserved VM-local SCMI path '{SCMI_PATH}' already exists"),
        });
    }
    let firmware = get_or_add_root_child(source, "firmware")?;
    let scmi_id = source.add_node(firmware, Node::new("scmi-65535"));
    let shmem_phandle = add_scmi_shared_memory(source, mediation.shared_memory())?;

    let first_phandle = next_phandle(source);
    let clock_phandle = (!mediation.clocks().is_empty()).then_some(first_phandle);
    let reset_phandle = if mediation.resets().is_empty() {
        None
    } else {
        Some(
            first_phandle
                .checked_add(u32::from(clock_phandle.is_some()))
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: "VM-local SCMI protocol phandle space is exhausted".into(),
                })?,
        )
    };
    let scmi = source
        .node_mut(scmi_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "VM-local SCMI node cannot be updated".into(),
        })?;
    scmi.set_property(string_property("compatible", "arm,scmi-smc"));
    scmi.set_property(u32_property("shmem", &[shmem_phandle]));
    scmi.set_property(u32_property("arm,smc-id", &[mediation.smc_function_id()]));
    scmi.set_property(u32_property("#address-cells", &[1]));
    scmi.set_property(u32_property("#size-cells", &[0]));

    if let Some(phandle) = clock_phandle {
        let protocol = source.add_node(scmi_id, Node::new("protocol@14"));
        let protocol =
            source
                .node_mut(protocol)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: "VM-local SCMI clock protocol cannot be updated".into(),
                })?;
        protocol.set_property(u32_property("reg", &[0x14]));
        protocol.set_property(u32_property("#clock-cells", &[1]));
        protocol.set_property(u32_property("phandle", &[phandle]));
    }
    if let Some(phandle) = reset_phandle {
        let protocol = source.add_node(scmi_id, Node::new("protocol@16"));
        let protocol =
            source
                .node_mut(protocol)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: "VM-local SCMI reset protocol cannot be updated".into(),
                })?;
        protocol.set_property(u32_property("reg", &[0x16]));
        protocol.set_property(u32_property("#reset-cells", &[1]));
        protocol.set_property(u32_property("phandle", &[phandle]));
    }
    Ok(MediationNodes {
        clock_phandle,
        reset_phandle,
    })
}

fn add_scmi_shared_memory(
    source: &mut Fdt,
    range: crate::machine::AddressRange,
) -> MachinePlanResult<u32> {
    let root = source
        .node(source.root_id())
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "guest FDT has no root node".into(),
        })?;
    let address_cells = root.address_cells().unwrap_or(2);
    let size_cells = root.size_cells().unwrap_or(1);
    if !(1..=2).contains(&address_cells) || !(1..=2).contains(&size_cells) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "unsupported root address/size cell widths {address_cells}/{size_cells} for \
                 VM-local SCMI shared memory"
            ),
        });
    }
    let reserved_memory = get_or_add_root_child(source, "reserved-memory")?;
    let reserved =
        source
            .node_mut(reserved_memory)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "guest reserved-memory node cannot be updated".into(),
            })?;
    reserved.set_property(u32_property("#address-cells", &[address_cells]));
    reserved.set_property(u32_property("#size-cells", &[size_cells]));
    reserved.set_property(Property::new("ranges", Vec::new()));

    let path = scmi_shared_memory_path(range);
    let name = path.rsplit_once('/').map(|(_, name)| name).ok_or_else(|| {
        MachinePlanError::InvalidFirmware {
            detail: "VM-local SCMI shared-memory path has no node name".into(),
        }
    })?;
    if source.get_by_path_id(&path).is_some() {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!("VM-local SCMI shared-memory path '{path}' already exists"),
        });
    }
    let phandle = next_phandle(source);
    let shmem = source.add_node(reserved_memory, Node::new(name));
    let node = source
        .node_mut(shmem)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "VM-local SCMI shared-memory node cannot be updated".into(),
        })?;
    node.set_property(string_property("compatible", "arm,scmi-shmem"));
    node.set_property(Property::new("no-map", Vec::new()));
    node.set_property(u32_property("phandle", &[phandle]));
    source
        .view_typed_mut(shmem)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "VM-local SCMI shared-memory node cannot encode its address".into(),
        })?
        .set_regs(&[RegInfo::new(range.base(), Some(range.size()))]);
    Ok(phandle)
}

fn scmi_shared_memory_path(range: crate::machine::AddressRange) -> String {
    format!("/reserved-memory/scmi-shmem@{:x}", range.base())
}

fn get_or_add_root_child(source: &mut Fdt, name: &str) -> MachinePlanResult<NodeId> {
    let path = format!("/{name}");
    if let Some(node) = source.get_by_path_id(&path) {
        return Ok(node);
    }
    let root = source.root_id();
    Ok(source.add_node(root, Node::new(name)))
}

fn missing_mediator(provider: &str, specifier: &[u32]) -> MachinePlanError {
    MachinePlanError::InvalidFirmware {
        detail: format!(
            "host provider '{provider}' selector {specifier:?} has no VM-local mediator resource"
        ),
    }
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}

fn u32_property(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}

pub(super) fn materialize_preconfigured_provider_resources(
    source: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<BTreeSet<String>> {
    let mediation = plan
        .provider_mediation()
        .map(|mediation| add_scmi_nodes(source, mediation))
        .transpose()?;
    for resources in plan.preconfigured_host_devices() {
        materialize_device_resources(source, plan, resources, mediation)?;
    }
    let mut vm_local_paths = BTreeSet::new();
    if let Some(mediation) = plan.provider_mediation() {
        vm_local_paths.insert(SCMI_PATH.into());
        vm_local_paths.insert("/reserved-memory".into());
        if !mediation.clocks().is_empty() {
            vm_local_paths.insert(format!("{SCMI_PATH}/protocol@14"));
        }
        if !mediation.resets().is_empty() {
            vm_local_paths.insert(format!("{SCMI_PATH}/protocol@16"));
        }
        vm_local_paths.insert(scmi_shared_memory_path(mediation.shared_memory()));
    }
    Ok(vm_local_paths)
}

fn materialize_device_resources(
    source: &mut Fdt,
    plan: &VmMachinePlan,
    resources: &PreconfiguredHostDeviceResources,
    mediation: Option<MediationNodes>,
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
    let clock_cells = materialize_clock_cells(
        path,
        source,
        plan,
        mediation,
        &references,
        resources.clocks(),
    )?;
    let clock_configuration = materialize_clock_configuration(
        path,
        source,
        plan,
        mediation,
        &references,
        resources.clock_configurations(),
    )?;
    let reset_rewrite = plan_reset_rewrite(
        path,
        source,
        plan,
        mediation,
        node_id,
        &references,
        resources.resets(),
    )?;
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
    match clock_configuration {
        ClockConfigurationRewrite::Unchanged => {}
        ClockConfigurationRewrite::Remove => {
            for property in [
                "assigned-clocks",
                "assigned-clock-parents",
                "assigned-clock-rates",
            ] {
                node.remove_property(property);
            }
        }
        ClockConfigurationRewrite::Replace(properties) => {
            for (name, cells) in properties {
                let mut property = Property::new(&name, Vec::new());
                property.set_u32_ls(&cells);
                node.set_property(property);
            }
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
    plan: &VmMachinePlan,
    mediation: Option<MediationNodes>,
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
            append_planned_clock(source, plan, mediation, &planned[index], &mut cells)?;
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

fn append_planned_clock(
    source: &mut Fdt,
    plan: &VmMachinePlan,
    mediation: Option<MediationNodes>,
    clock: &PreconfiguredHostClock,
    cells: &mut Vec<u32>,
) -> MachinePlanResult<()> {
    if let Some(rate_hz) = clock.rate_hz() {
        cells.push(add_fixed_clock(source, rate_hz.get())?);
        return Ok(());
    }
    let mediation_plan = plan
        .provider_mediation()
        .ok_or_else(|| missing_mediator(clock.provider().as_str(), clock.specifier()))?;
    let phandle = mediation
        .and_then(|nodes| nodes.clock_phandle)
        .ok_or_else(|| missing_mediator(clock.provider().as_str(), clock.specifier()))?;
    let id = mediation_plan
        .clock_id(clock.provider(), clock.specifier())
        .ok_or_else(|| missing_mediator(clock.provider().as_str(), clock.specifier()))?;
    cells.extend_from_slice(&[phandle, id]);
    Ok(())
}

enum ClockConfigurationRewrite {
    Unchanged,
    Remove,
    Replace(Vec<(String, Vec<u32>)>),
}

fn materialize_clock_configuration(
    path: &str,
    source: &mut Fdt,
    plan: &VmMachinePlan,
    mediation: Option<MediationNodes>,
    references: &[super::dependencies::FdtNodeDependency],
    planned: &[PreconfiguredHostClock],
) -> MachinePlanResult<ClockConfigurationRewrite> {
    if planned.is_empty() {
        return Ok(ClockConfigurationRewrite::Unchanged);
    }
    validate_clock_configuration_references(path, references, planned)?;
    if planned.iter().all(|clock| !clock.is_mediated()) {
        return Ok(ClockConfigurationRewrite::Remove);
    }
    if planned.iter().any(|clock| !clock.is_mediated()) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "host device '{path}' mixes fixed and mediated assigned-clock resources"
            ),
        });
    }

    let mut consumed = vec![false; planned.len()];
    let mut properties = Vec::new();
    for property_name in ["assigned-clocks", "assigned-clock-parents"] {
        let property_references = references
            .iter()
            .filter(|reference| reference.property() == property_name)
            .collect::<Vec<_>>();
        if property_references.is_empty() {
            continue;
        }
        let mut cells = Vec::new();
        for reference in property_references {
            let index = matching_clock_index(reference, planned, &consumed).ok_or_else(|| {
                MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "mediated clocks for host device '{path}' no longer match \
                         '{property_name}'"
                    ),
                }
            })?;
            consumed[index] = true;
            append_planned_clock(source, plan, mediation, &planned[index], &mut cells)?;
        }
        properties.push((String::from(property_name), cells));
    }
    if !consumed.iter().all(|consumed| *consumed) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "mediated assigned clocks for host device '{path}' no longer match the source FDT"
            ),
        });
    }
    Ok(ClockConfigurationRewrite::Replace(properties))
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
    plan: &VmMachinePlan,
    mediation: Option<MediationNodes>,
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
            if planned[index].is_mediated() {
                let mediation_plan = plan.provider_mediation().ok_or_else(|| {
                    missing_mediator(
                        planned[index].provider().as_str(),
                        planned[index].specifier(),
                    )
                })?;
                let phandle = mediation
                    .and_then(|nodes| nodes.reset_phandle)
                    .ok_or_else(|| {
                        missing_mediator(
                            planned[index].provider().as_str(),
                            planned[index].specifier(),
                        )
                    })?;
                let id = mediation_plan
                    .reset_id(planned[index].provider(), planned[index].specifier())
                    .ok_or_else(|| {
                        missing_mediator(
                            planned[index].provider().as_str(),
                            planned[index].specifier(),
                        )
                    })?;
                retained.push(ResetReference::Mediated { phandle, id });
                retained_indices.push(source_index);
            }
        } else {
            retained.push(ResetReference::Source(source_reset));
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
        match reset {
            ResetReference::Source(reset) => {
                cells.push(provider_phandle(source, reset.provider())?);
                cells.extend_from_slice(reset.reference().specifier());
            }
            ResetReference::Mediated { phandle, id } => {
                cells.extend_from_slice(&[phandle, id]);
            }
        }
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

enum ResetReference<'a> {
    Source(&'a super::dependencies::FdtNodeDependency),
    Mediated { phandle: u32, id: u32 },
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
