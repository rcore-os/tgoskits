//! Device graph declaration and deterministic topological sealing.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::ToString,
    vec::Vec,
};

use super::{resolved::ResolvedPciHost, *};
use crate::*;

/// Mutable architecture-owned device graph construction surface.
#[derive(Default)]
pub struct DeviceGraphBuilder {
    nodes: BTreeMap<DeviceNodeId, DeviceNodeSpec>,
    pci_hosts: BTreeMap<PciHostKey, DeclaredPciHost>,
}

pub(crate) struct DeclaredPciHost {
    pub(crate) host_id: DeviceNodeId,
    pub(crate) memory_aperture_slot: ResourceSlot,
    pub(crate) platform_functions: Vec<PciFunctionSpec>,
    pub(crate) reserved_bdfs: Vec<PciBdf>,
}

impl DeviceGraphBuilder {
    /// Creates an empty graph.
    pub const fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            pci_hosts: BTreeMap::new(),
        }
    }

    /// Adds one node, rejecting duplicate stable identities.
    pub fn add(&mut self, node: DeviceNodeSpec) -> Result<(), DeviceGraphError> {
        let id = node.id.clone();
        if self.nodes.contains_key(&id) {
            return Err(DeviceGraphError::DuplicateNode {
                node: id.to_string(),
            });
        }
        self.nodes.insert(id, node);
        Ok(())
    }

    /// Registers one typed PCI host provider and its ordinary graph node.
    pub fn register_pci_host(&mut self, provider: PciHostProvider) -> Result<(), DeviceGraphError> {
        if self.pci_hosts.contains_key(&provider.key) {
            return Err(DeviceGraphError::DuplicatePciHost {
                host: provider.key.to_string(),
            });
        }
        if provider.node.model.is_none() {
            return Err(DeviceGraphError::PciHostRequiresRuntimeModel {
                node: provider.node.id.to_string(),
            });
        }
        let host_id = provider.node.id().clone();
        self.add(provider.node)?;
        self.pci_hosts.insert(
            provider.key,
            DeclaredPciHost {
                host_id,
                memory_aperture_slot: provider.memory_aperture_slot,
                platform_functions: provider.platform_functions,
                reserved_bdfs: provider.reserved_bdfs,
            },
        );
        Ok(())
    }

    /// Returns the planning requests declared by the current unsealed graph.
    pub fn requests(&self) -> Result<Vec<DevicePlanRequest>, DeviceGraphError> {
        declared_nodes(&self.nodes).and_then(|nodes| {
            nodes
                .iter()
                .map(|node| DevicePlanRequest::new(node.id.as_str(), node.requirements.clone()))
                .collect::<DeviceManagerResult<Vec<_>>>()
                .map_err(|error| DeviceGraphError::Declaration {
                    node: "graph".into(),
                    detail: error.to_string(),
                })
        })
    }

    /// Runs every runtime factory's pure declaration phase and seals topology.
    pub fn declare(mut self) -> Result<DeclaredDeviceGraph, DeviceGraphError> {
        let requirements = self
            .nodes
            .iter()
            .map(|(id, node)| Ok((id.clone(), node.declared_requirements()?)))
            .collect::<Result<BTreeMap<_, _>, DeviceGraphError>>()?;
        add_pci_dependencies(&mut self.nodes, &self.pci_hosts, &requirements)?;
        let nodes = declared_nodes_with_requirements(&self.nodes, requirements)?;
        Ok(DeclaredDeviceGraph {
            nodes,
            pci_hosts: self.pci_hosts,
        })
    }
}

fn declared_nodes(
    nodes_by_id: &BTreeMap<DeviceNodeId, DeviceNodeSpec>,
) -> Result<Vec<DeclaredDeviceNode>, DeviceGraphError> {
    validate_edges(nodes_by_id)?;
    let order = topological_order(nodes_by_id)?;
    let mut nodes = Vec::with_capacity(order.len());
    for id in order {
        let mut node = nodes_by_id
            .get(&id)
            .expect("topological IDs originate from the graph")
            .to_declared()?;
        node.dependencies.sort();
        node.dependencies.dedup();
        nodes.push(node);
    }
    Ok(nodes)
}

fn declared_nodes_with_requirements(
    nodes_by_id: &BTreeMap<DeviceNodeId, DeviceNodeSpec>,
    mut requirements: BTreeMap<DeviceNodeId, DeviceRequirements>,
) -> Result<Vec<DeclaredDeviceNode>, DeviceGraphError> {
    validate_edges(nodes_by_id)?;
    let order = topological_order(nodes_by_id)?;
    let mut nodes = Vec::with_capacity(order.len());
    for id in order {
        let node = nodes_by_id
            .get(&id)
            .expect("topological IDs originate from the graph");
        let requirements = requirements
            .remove(&id)
            .expect("requirements were frozen for every graph node");
        let mut node = node.to_declared_with_requirements(requirements);
        node.dependencies.sort();
        node.dependencies.dedup();
        nodes.push(node);
    }
    Ok(nodes)
}

pub(crate) struct DeclaredDeviceNode {
    pub(crate) id: DeviceNodeId,
    pub(crate) kind: DeviceNodeKind,
    pub(crate) parent: Option<DeviceNodeId>,
    pub(crate) dependencies: Vec<DeviceNodeId>,
    pub(crate) firmware: super::DeviceFirmwareBinding,
    pub(crate) firmware_spec: DeviceFirmwareSpec,
    pub(crate) model: Option<alloc::sync::Arc<dyn crate::DeviceModel>>,
    pub(crate) requirements: DeviceRequirements,
    pub(crate) host_mapping: Option<super::HostPassthroughMapping>,
}

impl DeviceNodeSpec {
    pub(crate) fn declared_requirements(&self) -> Result<DeviceRequirements, DeviceGraphError> {
        if self.kind.requires_factory() && self.model.is_none() {
            return Err(DeviceGraphError::MissingFactory {
                node: self.id.to_string(),
            });
        }
        if self.kind == DeviceNodeKind::FirmwareOnly && self.model.is_some() {
            return Err(DeviceGraphError::FirmwareFactory {
                node: self.id.to_string(),
            });
        }
        self.firmware_spec
            .validate()
            .map_err(|error| DeviceGraphError::Declaration {
                node: self.id.to_string(),
                detail: error.to_string(),
            })?;
        let requirements = match (&self.model, &self.requirements) {
            (Some(model), _) => {
                model
                    .requirements()
                    .map_err(|error| DeviceGraphError::Declaration {
                        node: self.id.to_string(),
                        detail: error.to_string(),
                    })?
            }
            (None, Some(requirements)) => requirements.clone(),
            _ => {
                return Err(DeviceGraphError::Declaration {
                    node: self.id.to_string(),
                    detail: "node has inconsistent model and declaration state".into(),
                });
            }
        };
        if self.model.is_none() && requirements.pci_function().is_some() {
            return Err(DeviceGraphError::PciEndpointRequiresRuntimeModel {
                node: self.id.to_string(),
            });
        }
        Ok(requirements)
    }

    fn to_declared(&self) -> Result<DeclaredDeviceNode, DeviceGraphError> {
        let requirements = self.declared_requirements()?;
        Ok(self.to_declared_with_requirements(requirements))
    }

    fn to_declared_with_requirements(
        &self,
        requirements: DeviceRequirements,
    ) -> DeclaredDeviceNode {
        DeclaredDeviceNode {
            id: self.id.clone(),
            kind: self.kind,
            parent: self.parent.clone(),
            dependencies: self.dependencies.clone(),
            firmware: self.firmware.clone(),
            firmware_spec: self.firmware_spec.clone(),
            model: self.model.clone(),
            requirements,
            host_mapping: self.host_mapping,
        }
    }
}

/// Sealed declarations awaiting architecture-owned resource pools.
pub struct DeclaredDeviceGraph {
    nodes: Vec<DeclaredDeviceNode>,
    pci_hosts: BTreeMap<PciHostKey, DeclaredPciHost>,
}

impl DeclaredDeviceGraph {
    /// Returns planning requests in deterministic topological order.
    pub fn requests(&self) -> DeviceManagerResult<Vec<DevicePlanRequest>> {
        self.nodes
            .iter()
            .map(|node| DevicePlanRequest::new(node.id.as_str(), node.requirements.clone()))
            .collect()
    }

    /// Resolves resources and retains non-runtime fixed-node leases.
    pub fn resolve(self, pools: ResourcePools) -> DeviceManagerResult<ResolvedDeviceGraph> {
        let requests = self.requests()?;
        let plan = VmResourcePlanner::new(pools).plan(requests)?;
        let pci_topologies = resolve_pci_topologies(&self.nodes, &self.pci_hosts, &plan)?;
        let nodes = self
            .nodes
            .into_iter()
            .map(ResolvedDeviceNode::from_declared)
            .collect();
        ResolvedDeviceGraph::new(nodes, plan, pci_topologies)
    }
}

fn add_pci_dependencies(
    nodes: &mut BTreeMap<DeviceNodeId, DeviceNodeSpec>,
    providers: &BTreeMap<PciHostKey, DeclaredPciHost>,
    requirements: &BTreeMap<DeviceNodeId, DeviceRequirements>,
) -> Result<(), DeviceGraphError> {
    for (id, node) in nodes.iter_mut() {
        let Some(requirement) = requirements[id].pci_function() else {
            continue;
        };
        let provider = providers.get(requirement.host()).ok_or_else(|| {
            DeviceGraphError::PciHostUnavailable {
                endpoint: id.to_string(),
                host: requirement.host().to_string(),
            }
        })?;
        node.dependencies.push(provider.host_id.clone());
    }
    Ok(())
}

fn resolve_pci_topologies(
    nodes: &[DeclaredDeviceNode],
    providers: &BTreeMap<PciHostKey, DeclaredPciHost>,
    plan: &VmResourcePlan,
) -> DeviceManagerResult<BTreeMap<PciHostKey, ResolvedPciHost>> {
    let mut resolved = BTreeMap::new();
    for (key, provider) in providers {
        let (base, size) = plan
            .resources(provider.host_id.as_str())?
            .mmio(&provider.memory_aperture_slot)?;
        let end = base
            .checked_add(size)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "resolve PCI host aperture",
                detail: alloc::format!("host {key} memory aperture overflows u64"),
            })?;
        let mut topology = PciTopologyBuilder::new();
        for bdf in &provider.reserved_bdfs {
            topology.reserve_bdf(*bdf)?;
        }
        for function in &provider.platform_functions {
            topology.add_function(function.clone())?;
        }
        let mut endpoints = BTreeSet::new();
        for node in nodes {
            let Some(requirement) = node.requirements.pci_function() else {
                continue;
            };
            if requirement.host() == key {
                endpoints.insert(node.id.clone());
                topology.add_function(requirement.function_spec(node.id.clone())?)?;
            }
        }
        let mut topology = topology.resolve(base..end)?;
        topology.assign_graph_ownership(&provider.host_id, &endpoints);
        resolved.insert(
            key.clone(),
            ResolvedPciHost {
                host_id: provider.host_id.clone(),
                topology: alloc::sync::Arc::new(topology),
            },
        );
    }
    Ok(resolved)
}

fn validate_edges(nodes: &BTreeMap<DeviceNodeId, DeviceNodeSpec>) -> Result<(), DeviceGraphError> {
    for (id, node) in nodes {
        let mut seen = BTreeSet::new();
        for dependency in node.parent.iter().chain(node.dependencies.iter()) {
            if !nodes.contains_key(dependency) {
                return Err(DeviceGraphError::MissingDependency {
                    node: id.to_string(),
                    dependency: dependency.to_string(),
                });
            }
            if !seen.insert(dependency) {
                return Err(DeviceGraphError::DuplicateDependency {
                    node: id.to_string(),
                    dependency: dependency.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn topological_order(
    nodes: &BTreeMap<DeviceNodeId, DeviceNodeSpec>,
) -> Result<Vec<DeviceNodeId>, DeviceGraphError> {
    let mut incoming = nodes
        .iter()
        .map(|(id, node)| {
            let count = node.parent.iter().chain(node.dependencies.iter()).count();
            (id.clone(), count)
        })
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<DeviceNodeId, Vec<DeviceNodeId>>::new();
    for (id, node) in nodes {
        for dependency in node.parent.iter().chain(node.dependencies.iter()) {
            outgoing
                .entry(dependency.clone())
                .or_default()
                .push(id.clone());
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(id.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(id) = ready.pop_first() {
        order.push(id.clone());
        for dependent in outgoing.get(&id).into_iter().flatten() {
            let count = incoming
                .get_mut(dependent)
                .expect("outgoing edges originate from graph nodes");
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    if order.len() != nodes.len() {
        let node = incoming
            .iter()
            .find_map(|(id, count)| (*count != 0).then_some(id.to_string()))
            .expect("a shorter topological order leaves at least one node");
        return Err(DeviceGraphError::DependencyCycle { node });
    }
    Ok(order)
}
