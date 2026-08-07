//! Device graph declaration and deterministic topological sealing.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::ToString,
    vec::Vec,
};

use super::*;
use crate::*;

/// Mutable architecture-owned device graph construction surface.
#[derive(Default)]
pub struct DeviceGraphBuilder {
    nodes: BTreeMap<DeviceNodeId, DeviceNodeSpec>,
}

impl DeviceGraphBuilder {
    /// Creates an empty graph.
    pub const fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
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
    pub fn declare(self) -> Result<DeclaredDeviceGraph, DeviceGraphError> {
        let nodes = declared_nodes(&self.nodes)?;
        Ok(DeclaredDeviceGraph { nodes })
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

pub(crate) struct DeclaredDeviceNode {
    pub(crate) id: DeviceNodeId,
    pub(crate) kind: DeviceNodeKind,
    pub(crate) parent: Option<DeviceNodeId>,
    pub(crate) dependencies: Vec<DeviceNodeId>,
    pub(crate) firmware: super::DeviceFirmwareBinding,
    pub(crate) model: Option<alloc::sync::Arc<dyn crate::DeviceModel>>,
    pub(crate) requirements: DeviceRequirements,
    pub(crate) host_mapping: Option<super::HostPassthroughMapping>,
}

impl DeviceNodeSpec {
    fn to_declared(&self) -> Result<DeclaredDeviceNode, DeviceGraphError> {
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
        Ok(DeclaredDeviceNode {
            id: self.id.clone(),
            kind: self.kind,
            parent: self.parent.clone(),
            dependencies: self.dependencies.clone(),
            firmware: self.firmware.clone(),
            model: self.model.clone(),
            requirements,
            host_mapping: self.host_mapping,
        })
    }
}

/// Sealed declarations awaiting architecture-owned resource pools.
pub struct DeclaredDeviceGraph {
    nodes: Vec<DeclaredDeviceNode>,
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
        let nodes = self
            .nodes
            .into_iter()
            .map(ResolvedDeviceNode::from_declared)
            .collect();
        ResolvedDeviceGraph::new(nodes, plan)
    }
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
