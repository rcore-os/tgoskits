//! Immutable resolved device graph.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use super::{builder::*, *};
use crate::*;

/// One topologically ordered node in a resolved graph.
pub struct ResolvedDeviceNode {
    id: DeviceNodeId,
    kind: DeviceNodeKind,
    parent: Option<DeviceNodeId>,
    dependencies: Vec<DeviceNodeId>,
    firmware: DeviceFirmwareBinding,
    firmware_spec: DeviceFirmwareSpec,
    model: Option<Arc<dyn DeviceModel>>,
    host_mapping: Option<HostPassthroughMapping>,
    pci_host_topology: Option<Arc<ResolvedPciTopology>>,
    pci_endpoint: Option<ResolvedPciEndpoint>,
}

pub(crate) struct ResolvedPciEndpoint {
    pub(crate) host: DeviceNodeId,
    pub(crate) function_node: DeviceNodeId,
}

pub(crate) struct ResolvedPciHost {
    pub(crate) host_id: DeviceNodeId,
    pub(crate) topology: Arc<ResolvedPciTopology>,
}

impl ResolvedDeviceNode {
    pub(crate) fn from_declared(node: DeclaredDeviceNode) -> Self {
        Self {
            id: node.id,
            kind: node.kind,
            parent: node.parent,
            dependencies: node.dependencies,
            firmware: node.firmware,
            firmware_spec: node.firmware_spec,
            model: node.model,
            host_mapping: node.host_mapping,
            pci_host_topology: None,
            pci_endpoint: None,
        }
    }

    /// Returns this node's stable identity.
    pub const fn id(&self) -> &DeviceNodeId {
        &self.id
    }

    /// Returns its ownership and firmware semantics.
    pub const fn kind(&self) -> DeviceNodeKind {
        self.kind
    }

    /// Returns the optional firmware parent.
    pub const fn parent(&self) -> Option<&DeviceNodeId> {
        self.parent.as_ref()
    }

    /// Returns explicit construction dependencies.
    pub fn dependencies(&self) -> &[DeviceNodeId] {
        &self.dependencies
    }

    /// Returns normalized firmware identity.
    pub const fn firmware_binding(&self) -> &DeviceFirmwareBinding {
        &self.firmware
    }

    /// Returns the firmware declaration frozen when this node was created.
    pub const fn firmware(&self) -> &DeviceFirmwareSpec {
        &self.firmware_spec
    }

    /// Returns the exact model that declared this node.
    pub fn model(&self) -> Option<&Arc<dyn DeviceModel>> {
        self.model.as_ref()
    }

    /// Returns the normalized host mapping retained for a passthrough node.
    pub const fn host_mapping(&self) -> Option<HostPassthroughMapping> {
        self.host_mapping
    }

    pub(crate) const fn builds_at_runtime(&self) -> bool {
        self.model.is_some()
    }

    pub(crate) fn pci_host_topology(&self) -> Option<&Arc<ResolvedPciTopology>> {
        self.pci_host_topology.as_ref()
    }

    pub(crate) const fn pci_endpoint(&self) -> Option<&ResolvedPciEndpoint> {
        self.pci_endpoint.as_ref()
    }
}

/// One immutable graph and its single authoritative resource plan.
pub struct ResolvedDeviceGraph {
    nodes: Vec<ResolvedDeviceNode>,
    resources: VmResourcePlan,
    fixed_leases: Vec<ResourceLease>,
    pci_topologies: BTreeMap<PciHostKey, ResolvedPciHost>,
}

impl ResolvedDeviceGraph {
    pub(crate) fn new(
        mut nodes: Vec<ResolvedDeviceNode>,
        resources: VmResourcePlan,
        pci_topologies: BTreeMap<PciHostKey, ResolvedPciHost>,
    ) -> DeviceManagerResult<Self> {
        let mut fixed_leases = Vec::new();
        for node in nodes.iter().filter(|node| !node.builds_at_runtime()) {
            let slots = resources.resources(node.id.as_str())?.slots();
            let mut claims = resources.claim_device(node.id.as_str())?;
            for slot in slots {
                fixed_leases.push(claims.consume(&slot)?);
            }
            claims.finish()?;
        }
        for host in pci_topologies.values() {
            let host_node = nodes
                .iter_mut()
                .find(|node| node.id == host.host_id)
                .expect("resolved PCI hosts originate from graph nodes");
            host_node.pci_host_topology = Some(host.topology.clone());
            for function in host.topology.functions() {
                if function.owner() == &host.host_id {
                    continue;
                }
                let endpoint = nodes
                    .iter_mut()
                    .find(|node| node.id == *function.owner())
                    .expect("resolved PCI endpoint owners originate from graph nodes");
                endpoint.pci_endpoint = Some(ResolvedPciEndpoint {
                    host: host.host_id.clone(),
                    function_node: function.id().clone(),
                });
            }
        }
        Ok(Self {
            nodes,
            resources,
            fixed_leases,
            pci_topologies,
        })
    }

    /// Iterates nodes in deterministic dependency order.
    pub fn nodes(&self) -> impl Iterator<Item = &ResolvedDeviceNode> {
        self.nodes.iter()
    }

    /// Iterates host mappings in deterministic graph order.
    pub fn host_mappings(&self) -> impl Iterator<Item = HostPassthroughMapping> + '_ {
        self.nodes
            .iter()
            .filter_map(ResolvedDeviceNode::host_mapping)
    }

    /// Returns the resolved resources for one node.
    pub fn resources_for(
        &self,
        id: &DeviceNodeId,
    ) -> DeviceManagerResult<&ResolvedDeviceResources> {
        self.resources.resources(id.as_str())
    }

    /// Returns the canonical VM resource plan used by firmware and runtime.
    pub const fn resource_plan(&self) -> &VmResourcePlan {
        &self.resources
    }

    /// Returns the immutable PCI topology published for one typed host.
    pub fn pci_topology(&self, host: &PciHostKey) -> Option<&ResolvedPciTopology> {
        self.pci_topologies
            .get(host)
            .map(|host| host.topology.as_ref())
    }

    /// Rejects a runtime model that declares platform nodes but no FDT form.
    pub fn validate_fdt_support(&self) -> DeviceManagerResult {
        self.validate_firmware_support("FDT", DeviceFirmwareSpec::fdt)
    }

    /// Rejects a runtime model that declares platform nodes but no ACPI form.
    pub fn validate_acpi_support(&self) -> DeviceManagerResult {
        self.validate_firmware_support("ACPI", DeviceFirmwareSpec::acpi)
    }

    /// Returns the number of VM-lifetime reservations owned by non-runtime nodes.
    pub fn fixed_lease_count(&self) -> usize {
        self.fixed_leases.len()
    }

    fn validate_firmware_support<T: ?Sized>(
        &self,
        interface: &'static str,
        contributions: impl Fn(&DeviceFirmwareSpec) -> Option<&T>,
    ) -> DeviceManagerResult {
        for node in &self.nodes {
            if matches!(node.firmware(), DeviceFirmwareSpec::Interfaces { .. })
                && contributions(node.firmware()).is_none()
            {
                return Err(DeviceManagerError::Unsupported {
                    operation: "select guest firmware interface",
                    detail: alloc::format!("device {} does not support {interface}", node.id()),
                });
            }
        }
        Ok(())
    }
}
