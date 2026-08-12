//! VM-local device graphs created by architecture-owned initialization.

mod passthrough;
mod pools;

use core::ops::Range;
use std::vec::Vec;

use axdevice::*;

use crate::{AxVmResult, config::AxVMConfig};

/// One immutable graph and its one-shot resource claims.
pub(crate) struct VmDevicePlan {
    graph: ResolvedDeviceGraph,
}

impl VmDevicePlan {
    pub(crate) fn with_pools_for_vm(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        Self::build(config, nodes, replacement_ranges, &mut pools)
    }

    fn build(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        pools: &mut ResourcePools,
    ) -> AxVmResult<Self> {
        let mut builder = DeviceGraphBuilder::new();
        for node in nodes {
            builder.add(node).map_err(DeviceManagerError::from)?;
        }

        let configured_requests = builder.requests().map_err(DeviceManagerError::from)?;
        let fixed_internal_ranges = pools::fixed_mmio_ranges(&configured_requests)?;
        pools::reserve_guest_memory(config, pools)?;

        let mut replacement_ranges = replacement_ranges.to_vec();
        replacement_ranges.extend(fixed_internal_ranges);
        passthrough::add_host_nodes(config, &replacement_ranges, &mut builder)?;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axvm_types::{VmMemConfig, VmMemMappingType};

    use super::*;
    use crate::{
        AxVmError,
        config::{AxVMConfigParams, PhysCpuList},
    };

    struct FixedMmioInsideRamModel;

    impl DeviceModel for FixedMmioInsideRamModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new().with_mmio(
                ResourceSlot::new("registers")?,
                0x1000,
                0x1000,
                ResourceRequest::Fixed(0x8000_0000),
            )
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            Ok(DeviceBundle::new())
        }
    }

    #[test]
    fn fixed_mmio_inside_guest_memory_is_rejected() {
        let config = AxVMConfig::new(AxVMConfigParams {
            id: 1,
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            memory_regions: vec![VmMemConfig {
                gpa: 0x8000_0000,
                size: 0x2000,
                flags: 0x7,
                map_type: VmMemMappingType::MapIdentical,
            }],
            ..Default::default()
        });
        let nodes = vec![DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("bad-mmio").unwrap(),
            Arc::new(FixedMmioInsideRamModel),
        )];

        let error = VmDevicePlan::with_pools_for_vm(&config, nodes, &[], ResourcePools::new())
            .err()
            .expect("fixed device MMIO inside guest RAM must conflict");

        let AxVmError::Device { operation, detail } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(operation, "manage virtual devices");
        assert!(detail.contains("mmio resource 0x80000000..0x80001000"));
        assert!(detail.contains("requested by bad-mmio"));
        assert!(detail.contains("existing owner guest-memory-0"));
        assert!(detail.contains("address ranges overlap"));
    }
}
