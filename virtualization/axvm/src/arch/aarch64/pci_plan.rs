//! AArch64 generic-ECAM host construction and resolved firmware view.

use std::sync::Arc;

use axdevice::*;

use crate::{AxVmError, AxVmResult, boot::fdt::core::pci::GuestPciHost, config::AxVMConfig};

const PCI_HOST_ID: &str = "pci-host";
const PCI_HOST_KEY: &str = "aarch64-ecam";
const ECAM_SLOT: &str = "ecam";
const MEMORY_SLOT: &str = "memory-aperture";
const PCI_MEMORY_APERTURE_SIZE: u64 = 0x0400_0000;

pub(super) fn host_key() -> PciHostKey {
    PciHostKey::new(PCI_HOST_KEY).expect("static AArch64 PCI host key is valid")
}

pub(super) fn provider(controller: &DeviceNodeId) -> DeviceManagerResult<PciHostProvider> {
    let host_id = DeviceNodeId::new(PCI_HOST_ID)?;
    let model: Arc<dyn DeviceModel> = Arc::new(Aarch64PciHostModel {
        host_id: host_id.clone(),
    });
    let node = DeviceNodeSpec::virtual_device(host_id, model).with_dependency(controller.clone());
    Ok(PciHostProvider::new(
        host_key(),
        node,
        ResourceSlot::new(MEMORY_SLOT)?,
    ))
}

struct Aarch64PciHostModel {
    host_id: DeviceNodeId,
}

impl DeviceModel for Aarch64PciHostModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new(ECAM_SLOT)?,
                PCI_BUS_ZERO_ECAM_SIZE,
                PCI_BUS_ZERO_ECAM_SIZE,
                ResourceRequest::Auto,
            )?
            .with_mmio(
                ResourceSlot::new(MEMORY_SLOT)?,
                PCI_MEMORY_APERTURE_SIZE,
                PCI_MEMORY_APERTURE_SIZE,
                ResourceRequest::Auto,
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        // The AArch64 firmware adapter emits the generic ECAM node from the
        // graph-resolved host ranges after validating the input DTB.
        DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let ecam = context.mmio(ECAM_SLOT)?;
        let memory = context.mmio(MEMORY_SLOT)?;
        let memory_end =
            memory
                .0
                .checked_add(memory.1)
                .ok_or_else(|| DeviceManagerError::InvalidConfig {
                    operation: "build AArch64 PCI host",
                    detail: "resolved PCI memory aperture overflows u64".into(),
                })?;
        let topology = context
            .pci_host_topology()
            .ok_or_else(|| DeviceManagerError::InvalidState {
                operation: "build AArch64 PCI host",
                detail: "resolved graph did not attach PCI topology metadata".into(),
            })?
            .clone();
        if ecam.1 != PCI_BUS_ZERO_ECAM_SIZE
            || ecam.0 & (PCI_BUS_ZERO_ECAM_SIZE - 1) != 0
            || topology.memory_aperture() != &(memory.0..memory_end)
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build AArch64 PCI host",
                detail: "resolved PCI host resources differ from the AArch64 provider".into(),
            });
        }

        let root = Arc::new(PciRootState::new(topology));
        let binding = Arc::new(PciRootBinding::new(self.host_id.clone(), root.clone()));
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(PciEcamFrontend::new(ecam.0, root.clone())));
        bundle.add_device(Arc::new(PciMmioApertureDevice::new(
            memory.0,
            memory.1,
            binding.clone(),
        )));
        bundle.add_lifecycle(Arc::new(PciRootStateLifecycle::new(binding.clone())));
        bundle.provide_service::<PciRootBindingKey>(binding)?;
        Ok(bundle)
    }
}

#[derive(Debug)]
pub(super) struct Aarch64PciPlan {
    firmware: GuestPciHost,
}

impl Aarch64PciPlan {
    pub(super) fn resolve(
        config: &AxVMConfig,
        graph: &ResolvedDeviceGraph,
    ) -> AxVmResult<Option<Self>> {
        let Some(topology) = graph.pci_topology(&host_key()) else {
            return Ok(None);
        };
        let host_id = DeviceNodeId::new(PCI_HOST_ID)?;
        if !topology
            .functions()
            .any(|function| function.owner() != &host_id)
        {
            return Err(AxVmError::invalid_config(
                "AArch64 device graph materialized a PCI host without endpoints",
            ));
        }
        if config.image_config().dtb_load_gpa.is_none() {
            return Err(AxVmError::unsupported(
                "create AArch64 virtual PCI host",
                "configured PCI endpoints require a guest DTB; UEFI/ACPI PCI is not implemented",
            ));
        }

        let resources = graph.resources_for(&host_id)?;
        let ecam = resources.mmio(&ResourceSlot::new(ECAM_SLOT)?)?;
        let memory = resources.mmio(&ResourceSlot::new(MEMORY_SLOT)?)?;
        let firmware = GuestPciHost::new(ecam, memory)?;
        Ok(Some(Self { firmware }))
    }

    pub(super) const fn firmware(&self) -> GuestPciHost {
        self.firmware
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult, Resource};
    use axvm_types::GuestPhysAddr;

    use super::*;
    use crate::{
        config::{AxVMConfigParams, PhysCpuList, VMImageConfig},
        vm::prepare::device_plan::VmDevicePlan,
    };

    struct TestEndpointModel;
    struct TestFunction;

    impl DeviceModel for TestEndpointModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            let bar = PciMemoryBar::new(PciBarIndex::new(2)?, 0x1_0000)?;
            let function = PciFunctionRequirement::new(
                host_key(),
                PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0x05, 0, 0)),
            )
            .with_bar(bar)?;
            DeviceRequirements::new().with_pci_function(function)
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            let mut bundle = DeviceBundle::new();
            bundle.add_pci_function(Arc::new(TestFunction))?;
            Ok(bundle)
        }
    }

    impl Device for TestFunction {
        fn name(&self) -> &str {
            "aarch64-test-pci"
        }

        fn resources(&self) -> &[Resource] {
            &[]
        }

        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            Err(DeviceError::NotFound)
        }

        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Err(DeviceError::NotFound)
        }
    }

    impl PciFunction for TestFunction {
        fn read_bar(
            &self,
            _access: PciBarAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            Ok(0)
        }

        fn write_bar(
            &self,
            _access: PciBarAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Ok(())
        }
    }

    fn config(with_dtb: bool) -> AxVMConfig {
        AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            image_config: VMImageConfig {
                dtb_load_gpa: with_dtb.then(|| GuestPhysAddr::from_usize(0x4000_0000)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[cfg(target_arch = "aarch64")]
    fn auto_mmio_search() -> core::ops::Range<u64> {
        super::super::resource_pools::AUTO_MMIO_SEARCH.clone()
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn auto_mmio_search() -> core::ops::Range<u64> {
        0x0b00_0000..0x1_0000_0000
    }

    fn device_plan(
        with_endpoint: bool,
        reservations: &[(&str, core::ops::Range<u64>)],
    ) -> AxVmResult<VmDevicePlan> {
        let controller = DeviceNodeId::new("vgic").unwrap();
        let mut nodes = std::vec![DeviceNodeSpec::firmware_only(controller.clone())];
        if with_endpoint {
            nodes.push(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("endpoint0").unwrap(),
                Arc::new(TestEndpointModel),
            ));
        }
        let mut pools = ResourcePools::new();
        pools.add_auto_mmio(auto_mmio_search())?;
        for (owner, range) in reservations {
            pools.reserve_mmio((*owner).to_string(), range.clone())?;
        }
        VmDevicePlan::with_optional_pci_host_for_vm(
            &config(with_endpoint),
            nodes,
            &[],
            pools,
            provider(&controller)?,
        )
    }

    #[test]
    fn endpoint_resolves_one_generic_ecam_firmware_view_and_runtime_root() {
        let devices = device_plan(true, &[]).unwrap();
        let graph = devices.graph();
        let ids = graph
            .nodes()
            .map(|node| node.id().as_str())
            .collect::<std::vec::Vec<_>>();
        assert_eq!(ids, ["vgic", "pci-host", "endpoint0"]);

        let plan = Aarch64PciPlan::resolve(&config(true), graph)
            .unwrap()
            .unwrap();
        let firmware = plan.firmware();
        assert_eq!(firmware.ecam_base(), 0x0b00_0000);
        assert_eq!(firmware.memory_base(), 0x0c00_0000);
        assert_eq!(firmware.memory_size(), PCI_MEMORY_APERTURE_SIZE);

        let mut runtime = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
        for node in graph.nodes() {
            runtime
                .build_graph_node(node, graph.resource_plan())
                .unwrap();
        }
        runtime.finish(graph.resource_plan()).unwrap();
    }

    #[test]
    fn host_without_endpoints_is_not_materialized() {
        let devices = device_plan(false, &[]).unwrap();
        let ids = devices
            .graph()
            .nodes()
            .map(|node| node.id().as_str())
            .collect::<std::vec::Vec<_>>();

        assert_eq!(ids, ["vgic"]);
        assert!(
            Aarch64PciPlan::resolve(&config(false), devices.graph())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn endpoint_aperture_skips_active_low_mmio_reservations() {
        let devices = device_plan(
            true,
            &[
                ("active-minidump", 0x0c00_0000..0x0e00_0000),
                ("active-cma", 0x1000_0000..0x2000_0000),
            ],
        )
        .unwrap();
        let resources = devices
            .graph()
            .resources_for(&DeviceNodeId::new(PCI_HOST_ID).unwrap())
            .unwrap();

        assert_eq!(
            resources.mmio(&ResourceSlot::new(MEMORY_SLOT).unwrap()),
            Ok((0x2000_0000, PCI_MEMORY_APERTURE_SIZE))
        );
    }

    #[test]
    fn endpoint_aperture_fails_when_no_aligned_window_exists_below_four_gib() {
        let error = device_plan(
            true,
            &[("occupied-32-bit-mmio", 0x0c00_0000..auto_mmio_search().end)],
        )
        .err()
        .expect("the PCI aperture must not move above 4 GiB");

        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(detail.contains("mmio auto pool is exhausted"));
        assert!(detail.contains("slot memory-aperture for pci-host"));
    }

    #[test]
    fn endpoint_without_guest_dtb_is_rejected() {
        let devices = device_plan(true, &[]).unwrap();
        let error = Aarch64PciPlan::resolve(&config(false), devices.graph()).unwrap_err();
        assert!(matches!(error, AxVmError::Unsupported { .. }));
        assert!(error.to_string().contains("require a guest DTB"));
    }
}
