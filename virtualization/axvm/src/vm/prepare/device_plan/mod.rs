//! VM-local device graphs created by architecture-owned initialization.

mod passthrough;
mod pools;

use alloc::vec::Vec;
use core::ops::Range;

use axdevice::{
    DeviceGraphBuilder, DeviceManagerError, DeviceNodeSpec, ResolvedDeviceGraph, ResourcePools,
};

use crate::{AxVmResult, config::AxVMConfig};

/// One immutable graph and its one-shot resource claims.
pub(crate) struct VmDevicePlan {
    graph: ResolvedDeviceGraph,
}

impl VmDevicePlan {
    #[cfg(test)]
    pub(crate) fn fixed(nodes: Vec<DeviceNodeSpec>) -> AxVmResult<Self> {
        Self::with_pools(nodes, ResourcePools::new())
    }

    #[cfg(test)]
    pub(crate) fn with_pools(
        nodes: Vec<DeviceNodeSpec>,
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        Self::build(None, nodes, &[], &mut pools)
    }

    pub(crate) fn with_pools_for_vm(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        pools::reserve_guest_memory(config, &mut pools)?;
        Self::build(Some(config), nodes, replacement_ranges, &mut pools)
    }

    fn build(
        vm_config: Option<&AxVMConfig>,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        pools: &mut ResourcePools,
    ) -> AxVmResult<Self> {
        let mut builder = DeviceGraphBuilder::new();
        for node in nodes {
            builder.add(node).map_err(DeviceManagerError::from)?;
        }

        if let Some(config) = vm_config {
            passthrough::add_host_nodes(config, replacement_ranges, &mut builder)?;
        }

        let declared = builder.declare().map_err(DeviceManagerError::from)?;
        let requests = declared.requests()?;
        pools::allow_fixed_requirements(&requests, pools)?;
        let graph = declared.resolve(core::mem::take(pools))?;
        Ok(Self { graph })
    }

    pub(crate) const fn graph(&self) -> &ResolvedDeviceGraph {
        &self.graph
    }
}

/// Small common capability exposed by every architecture-specific VM plan.
pub(crate) trait ArchitectureVmPlan {
    fn devices(&self) -> &VmDevicePlan;
}

/// Plan used by architectures with no extra immutable controller metadata.
pub(crate) struct SimpleVmPlan(VmDevicePlan);

impl SimpleVmPlan {
    pub(crate) const fn new(devices: VmDevicePlan) -> Self {
        Self(devices)
    }
}

impl ArchitectureVmPlan for SimpleVmPlan {
    fn devices(&self) -> &VmDevicePlan {
        &self.0
    }
}
