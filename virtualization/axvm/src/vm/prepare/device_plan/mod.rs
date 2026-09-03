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
    #[cfg(any(not(target_arch = "x86_64"), test))]
    pub(crate) fn with_pools_for_vm(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        mut pools: ResourcePools,
    ) -> AxVmResult<Self> {
        Self::build(config, nodes, replacement_ranges, &mut pools, None)
    }

    pub(crate) fn with_pci_host_for_vm(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        mut pools: ResourcePools,
        pci_host: PciHostProvider,
    ) -> AxVmResult<Self> {
        Self::build(
            config,
            nodes,
            replacement_ranges,
            &mut pools,
            Some(pci_host),
        )
    }

    fn build(
        config: &AxVMConfig,
        nodes: Vec<DeviceNodeSpec>,
        replacement_ranges: &[Range<u64>],
        pools: &mut ResourcePools,
        pci_host: Option<PciHostProvider>,
    ) -> AxVmResult<Self> {
        let mut builder = DeviceGraphBuilder::new();
        for node in nodes {
            builder.add(node).map_err(DeviceManagerError::from)?;
        }
        if let Some(pci_host) = pci_host {
            builder
                .register_pci_host(pci_host)
                .map_err(DeviceManagerError::from)?;
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

    use axdevice_base::{ControllerInputId, InterruptControllerId};
    use axvm_types::{VmMemConfig, VmMemMappingType};
    use axvmconfig::VirtualDeviceRequest;

    use super::*;
    use crate::{
        AxVmError, ConfiguredDeviceCatalog,
        config::{AxVMConfigParams, PhysCpuList},
        configured::append_configured_devices,
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

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            Ok(DeviceBundle::new())
        }
    }

    struct FixedMmioOccupantModel;

    impl DeviceModel for FixedMmioOccupantModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new().with_mmio(
                ResourceSlot::new("registers")?,
                0x1_0000,
                0x1000,
                ResourceRequest::Fixed(0x1000_0000),
            )
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            Ok(DeviceBundle::new())
        }
    }

    fn registered_catalog() -> Arc<ConfiguredDeviceCatalog> {
        let mut catalog = ConfiguredDeviceCatalog::new();
        crate::machine::register_devices(&mut catalog).unwrap();
        Arc::new(catalog)
    }

    fn config_with_ivc() -> AxVMConfig {
        AxVMConfig::new(AxVMConfigParams {
            id: 1,
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            virtual_device_catalog: registered_catalog(),
            memory_regions: vec![VmMemConfig {
                gpa: 0x8000_0000,
                size: 0x1000_0000,
                flags: 0x7,
                map_type: VmMemMappingType::MapIdentical,
            }],
            virtual_device_requests: vec![VirtualDeviceRequest {
                id: "ivc0".into(),
                model: "ivc-channel".into(),
                options: Default::default(),
            }],
            ..Default::default()
        })
    }

    fn ivc_nodes(config: &AxVMConfig) -> Vec<DeviceNodeSpec> {
        let controller = DeviceNodeId::new("controller").unwrap();
        let mut nodes = vec![DeviceNodeSpec::firmware_only(controller.clone())];
        append_configured_devices(
            config,
            &mut nodes,
            &controller,
            InterruptControllerId::new(0),
        )
        .unwrap();
        nodes
    }

    #[test]
    fn ivc_mmio_aperture_is_planned_outside_guest_ram() {
        let config = config_with_ivc();
        let nodes = ivc_nodes(&config);
        let mut pools = ResourcePools::new();
        pools
            .add_auto_mmio(
                0x1000_0000..0x1000_0000 + crate::runtime::ivc::MAX_IVC_CHANNEL_SIZE as u64,
            )
            .unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(32)..ControllerInputId::new(36),
            )
            .unwrap();
        let plan = VmDevicePlan::with_pools_for_vm(&config, nodes, &[], pools).unwrap();
        let registers = ResourceSlot::new("registers").unwrap();
        let (base, size) = plan
            .graph()
            .resources_for(&DeviceNodeId::new("ivc0").unwrap())
            .unwrap()
            .mmio(&registers)
            .unwrap();

        assert_eq!(
            (base, size),
            (
                0x1000_0000,
                crate::runtime::ivc::MAX_IVC_CHANNEL_SIZE as u64,
            )
        );
        for memory in config.memory_regions() {
            let memory_base = memory.gpa as u64;
            let memory_end = memory_base + memory.size as u64;
            assert!(
                base + size <= memory_base || memory_end <= base,
                "IVC MMIO aperture {base:#x}..{:#x} overlaps System RAM {:#x}..{memory_end:#x}",
                base + size,
                memory_base
            );
        }
    }

    #[test]
    fn ivc_mmio_aperture_uses_the_shared_mmio_allocator() {
        let config = config_with_ivc();
        let mut nodes = ivc_nodes(&config);
        nodes.push(DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("mmio-occupant").unwrap(),
            Arc::new(FixedMmioOccupantModel),
        ));
        let mut pools = ResourcePools::new();
        pools
            .add_auto_mmio(
                0x1000_0000..0x1000_0000 + crate::runtime::ivc::MAX_IVC_CHANNEL_SIZE as u64,
            )
            .unwrap();
        pools.allow_fixed_mmio(0x1000_0000..0x1001_0000).unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(32)..ControllerInputId::new(36),
            )
            .unwrap();

        let error = VmDevicePlan::with_pools_for_vm(&config, nodes, &[], pools)
            .err()
            .expect("IVC must allocate from the common MMIO aperture pool");
        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(detail.contains("mmio auto pool is exhausted"));
        assert!(detail.contains("slot registers for ivc0"));
    }

    #[test]
    fn ivc_notify_irq_is_allocated_from_the_machine_irq_domain() {
        let config = config_with_ivc();
        let nodes = ivc_nodes(&config);
        let mut pools = ResourcePools::new();
        pools
            .add_auto_mmio(
                0x1000_0000..0x1000_0000 + crate::runtime::ivc::MAX_IVC_CHANNEL_SIZE as u64,
            )
            .unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(32)..ControllerInputId::new(36),
            )
            .unwrap();

        let plan = VmDevicePlan::with_pools_for_vm(&config, nodes, &[], pools).unwrap();
        let notify = ResourceSlot::new("notify").unwrap();
        let irq = plan
            .graph()
            .resources_for(&DeviceNodeId::new("ivc0").unwrap())
            .unwrap()
            .wired_irq(&notify)
            .unwrap();

        assert_eq!(irq.input(), ControllerInputId::new(32));
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
