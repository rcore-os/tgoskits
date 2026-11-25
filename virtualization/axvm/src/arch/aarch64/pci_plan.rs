//! AArch64 generic-ECAM host construction and resolved firmware view.

use std::sync::Arc;

use axdevice::*;

use crate::{
    AxVmError, AxVmResult,
    boot::fdt::{
        core::pci::{GuestPciHost, PCI_BUS_ZERO_ECAM_SIZE},
        device::{ResolvedFdtSpecial, ResolvedFdtSpecialKind},
    },
    config::AxVMConfig,
};

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
        DeviceFirmwareSpec::interfaces(
            Some(vec![FdtContributionSpec::PciHostBridge(
                FdtNodeSpec::new("pci")
                    .with_compatible("pci-host-ecam-generic")
                    .with_register(ResourceSlot::new(ECAM_SLOT).expect("static ECAM slot is valid"))
                    .with_register(
                        ResourceSlot::new(MEMORY_SLOT).expect("static memory slot is valid"),
                    ),
            )]),
            None,
        )
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
        let binding = Arc::new(PciRootBinding::new(self.host_id.clone(), root));
        let mut bundle = DeviceBundle::new();
        bundle.add_device(Arc::new(PciEcamConfigFrontend::try_new(
            ecam.0,
            ecam.1,
            binding.clone(),
        )?));
        bundle.add_device(Arc::new(PciMemoryApertureDevice::new(
            memory.0,
            memory.1,
            binding.clone(),
        )));
        bundle.add_lifecycle(Arc::new(PciRootLifecycle::new(binding.clone())));
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
        specials: &mut Vec<ResolvedFdtSpecial>,
    ) -> AxVmResult<Option<Self>> {
        let mut hosts = specials
            .iter()
            .enumerate()
            .filter(|(_, special)| special.kind == ResolvedFdtSpecialKind::PciHostBridge);
        let host_index = hosts.next().map(|(index, _)| index);
        if hosts.next().is_some() {
            return Err(AxVmError::unsupported(
                "resolve AArch64 PCI firmware",
                "multiple PCI host contributions",
            ));
        }
        let topology = graph.pci_topology(&host_key());
        let (Some(topology), Some(host_index)) = (topology, host_index) else {
            if topology.is_some() || host_index.is_some() {
                return Err(AxVmError::invalid_config(
                    "AArch64 PCI topology and firmware contribution must exist together",
                ));
            }
            return Ok(None);
        };
        let host = &specials[host_index];
        let host_id = DeviceNodeId::new(&host.id)?;
        if topology
            .functions()
            .any(|function| function.host() != &host_id)
            || !topology
                .functions()
                .any(|function| function.owner() != &host_id)
        {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI contribution does not own the resolved endpoint topology",
            ));
        }
        if config.image_config().dtb_load_gpa.is_none() {
            return Err(AxVmError::unsupported(
                "create AArch64 virtual PCI host",
                "configured PCI endpoints require a guest DTB; UEFI/ACPI PCI is not implemented",
            ));
        }

        let [ecam, memory] = host.registers.as_slice() else {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI contribution requires ECAM and memory windows",
            ));
        };
        let firmware = GuestPciHost::new(*ecam, *memory)?;
        if host.node_name != "pci"
            || host.compatible.as_slice() != ["pci-host-ecam-generic"]
            || !host.interrupts.is_empty()
            || !host.properties.is_empty()
            || memory.1 != PCI_MEMORY_APERTURE_SIZE
            || topology.memory_aperture() != &(memory.0..memory.0 + memory.1)
        {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI contribution differs from the runtime host",
            ));
        }
        specials.remove(host_index);
        Ok(Some(Self { firmware }))
    }

    pub(super) const fn firmware(&self) -> GuestPciHost {
        self.firmware
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{
        Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult, InterruptControllerId,
        ItsId, LpiId, MsiDeviceId, MsiEventId, Resource,
    };
    use axvm_types::GuestPhysAddr;

    use super::*;
    use crate::{
        config::{AxVMConfigParams, PhysCpuList, VMImageConfig},
        vm::prepare::device_plan::VmDevicePlan,
    };

    struct TestEndpointModel;
    struct TestAutoMmioModel {
        size: u64,
    }
    struct TestMsiEndpointModel {
        device: u16,
        its: ItsId,
    }
    struct TestFunction;

    impl DeviceModel for TestEndpointModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            let bar = PciMemoryBar::new(PciBarIndex::new(2)?, 0x1_0000)?;
            let function = PciFunctionRequirement::new(
                host_key(),
                PciEndpointIdentity::new(0x1234, 0x5678, PciClass::new(0x05, 0, 0)),
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

    impl DeviceModel for TestAutoMmioModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new().with_mmio(
                ResourceSlot::new("registers")?,
                self.size,
                0x1000,
                ResourceRequest::Auto,
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

    impl DeviceModel for TestMsiEndpointModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            let function = PciFunctionRequirement::new(
                host_key(),
                PciEndpointIdentity::new(0x1234, self.device, PciClass::new(0x05, 0, 0)),
            );
            let msi = MsiResourceRequest::new(
                InterruptControllerId::new(0),
                self.its,
                1,
                ResourceRequest::Auto,
                ResourceRequest::Auto,
                ResourceRequest::Auto,
            )?;
            DeviceRequirements::new()
                .with_pci_function(function)?
                .with_msi(ResourceSlot::new("msi")?, msi)
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
            _context: &mut dyn PciEndpointContext,
        ) -> DeviceResult<u64> {
            Ok(0)
        }

        fn write_bar(
            &self,
            _access: PciBarAccess,
            _value: u64,
            _context: &mut dyn PciEndpointContext,
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

    fn pci_auto_mmio_search_end() -> u64 {
        super::super::resource_pools::PCI_AUTO_MMIO_SEARCH_EXTENSION.end
    }

    fn resolve_plan(
        config: &AxVMConfig,
        graph: &ResolvedDeviceGraph,
    ) -> AxVmResult<Option<Aarch64PciPlan>> {
        let mut firmware = crate::boot::fdt::device::resolve_fdt_firmware(graph)?;
        Aarch64PciPlan::resolve(config, graph, &mut firmware.specials)
    }

    fn device_plan(
        with_endpoint: bool,
        reservations: &[(&str, core::ops::Range<u64>)],
    ) -> AxVmResult<VmDevicePlan> {
        let extra_nodes = with_endpoint
            .then(|| {
                DeviceNodeSpec::virtual_device(
                    DeviceNodeId::new("endpoint0").unwrap(),
                    Arc::new(TestEndpointModel),
                )
            })
            .into_iter()
            .collect();
        device_plan_with_nodes(&config(with_endpoint), extra_nodes, reservations)
    }

    fn auto_mmio_device_plan(
        config: &AxVMConfig,
        reservations: &[(&str, core::ops::Range<u64>)],
    ) -> AxVmResult<VmDevicePlan> {
        device_plan_with_nodes(
            config,
            std::vec![DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("auto-mmio0").unwrap(),
                Arc::new(TestAutoMmioModel { size: 0x1000 }),
            )],
            reservations,
        )
    }

    fn device_plan_with_nodes(
        config: &AxVMConfig,
        extra_nodes: std::vec::Vec<DeviceNodeSpec>,
        reservations: &[(&str, core::ops::Range<u64>)],
    ) -> AxVmResult<VmDevicePlan> {
        let controller = DeviceNodeId::new("vgic").unwrap();
        let mut nodes = std::vec![DeviceNodeSpec::firmware_only(controller.clone())];
        nodes.extend(extra_nodes);
        let mut pools = super::super::resource_pools::create_auto_mmio_pools()?;
        for (owner, range) in reservations {
            pools.reserve_mmio((*owner).to_string(), range.clone())?;
        }
        VmDevicePlan::with_optional_pci_host_for_vm(
            config,
            nodes,
            &[],
            pools,
            provider(&controller)?,
            super::super::resource_pools::PCI_AUTO_MMIO_SEARCH_EXTENSION.clone(),
        )
    }

    fn msi_device_plan(endpoints: &[(&str, ItsId)]) -> VmDevicePlan {
        let config = config(true);
        let controller = DeviceNodeId::new("vgic").unwrap();
        let mut nodes = std::vec![DeviceNodeSpec::firmware_only(controller.clone())];
        nodes.extend(endpoints.iter().enumerate().map(|(index, (id, its))| {
            DeviceNodeSpec::virtual_device(
                DeviceNodeId::new(*id).unwrap(),
                Arc::new(TestMsiEndpointModel {
                    device: u16::try_from(index + 1).unwrap(),
                    its: *its,
                }),
            )
        }));

        let interrupt_controller = InterruptControllerId::new(0);
        let mut pools = super::super::resource_pools::create_auto_mmio_pools().unwrap();
        for its in [ItsId::new(0), ItsId::new(1)] {
            pools
                .add_auto_msi_domain(
                    interrupt_controller,
                    its,
                    MsiDeviceId::new(0)..MsiDeviceId::new(0x100),
                    MsiEventId::new(0)..MsiEventId::new(0x100),
                    LpiId::new(0x8000)..LpiId::new(0x8100),
                )
                .unwrap();
        }
        VmDevicePlan::with_optional_pci_host_for_vm(
            &config,
            nodes,
            &[],
            pools,
            provider(&controller).unwrap(),
            super::super::resource_pools::PCI_AUTO_MMIO_SEARCH_EXTENSION.clone(),
        )
        .unwrap()
    }

    fn two_its_profile() -> crate::machine::GuestGicProfile {
        use crate::machine::{
            GuestGicCpuRegion, GuestGicProfile, GuestGicRedistributorProfile, GuestItsProfile,
            GuestMmioRegion,
        };

        GuestGicProfile {
            compatible: "arm,gic-v3".into(),
            node_path: "/interrupt-controller@8000000".into(),
            node_phandle: Some(1),
            distributor: GuestMmioRegion {
                base: 0x0800_0000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                regions: std::vec![GuestMmioRegion {
                    base: 0x080a_0000,
                    length: 0x2_0000,
                }],
                stride: 0x2_0000,
            }),
            its: std::vec![
                GuestItsProfile {
                    id: ItsId::new(0),
                    node_path: "/its@8080000".into(),
                    node_phandle: Some(3),
                    registers: GuestMmioRegion {
                        base: 0x0808_0000,
                        length: 0x2_0000,
                    },
                },
                GuestItsProfile {
                    id: ItsId::new(1),
                    node_path: "/its@8082000".into(),
                    node_phandle: Some(7),
                    registers: GuestMmioRegion {
                        base: 0x0808_2000,
                        length: 0x2_0000,
                    },
                },
            ],
        }
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

        let plan = resolve_plan(&config(true), graph).unwrap().unwrap();
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
    fn pci_msi_parent_uses_resolved_endpoint_its_and_rejects_conflicts() {
        let controller = InterruptControllerId::new(0);
        let gic = two_its_profile();

        let mut single_its = gic.clone();
        single_its.its.truncate(1);
        let devices = device_plan(true, &[]).unwrap();
        assert_eq!(
            super::super::firmware_plan::resolve_pci_msi_parent(
                devices.graph(),
                controller,
                &single_its,
            ),
            Ok(Some(3))
        );

        let devices = device_plan(true, &[]).unwrap();
        assert_eq!(
            super::super::firmware_plan::resolve_pci_msi_parent(devices.graph(), controller, &gic),
            Ok(None)
        );

        let devices = msi_device_plan(&[("endpoint0", ItsId::new(1))]);
        assert_eq!(
            super::super::firmware_plan::resolve_pci_msi_parent(devices.graph(), controller, &gic),
            Ok(Some(7))
        );

        let devices =
            msi_device_plan(&[("endpoint0", ItsId::new(0)), ("endpoint1", ItsId::new(1))]);
        let error =
            super::super::firmware_plan::resolve_pci_msi_parent(devices.graph(), controller, &gic)
                .unwrap_err();
        assert!(matches!(&error, AxVmError::ResourceConflict { .. }));
        assert!(error.to_string().contains("generated PCI bus"));
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
            resolve_plan(&config(false), devices.graph())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn vm_without_pci_host_limits_auto_mmio_to_legacy_window() {
        const PROVIDED_DTB_STATIC_MMIO_BASE: u64 = 0x1000_0000;

        // This planning-level test intentionally does not parse provided DTB
        // bytes. A static device at 0x1000_0000 is outside memory_regions(),
        // so occupying the legacy window models that unregistered reservation.
        let plan = auto_mmio_device_plan(
            &config(true),
            &[(
                "provided-dtb-low-mmio-occupant",
                0x0b00_0000..PROVIDED_DTB_STATIC_MMIO_BASE,
            )],
        );
        let error = match plan {
            Ok(devices) => {
                let resources = devices
                    .graph()
                    .resources_for(&DeviceNodeId::new("auto-mmio0").unwrap())
                    .unwrap();
                let (base, _) = resources
                    .mmio(&ResourceSlot::new("registers").unwrap())
                    .unwrap();
                panic!(
                    "non-PCI VM allocated automatic MMIO at {base:#x}, beyond the legacy window \
                     ending at {PROVIDED_DTB_STATIC_MMIO_BASE:#x}"
                );
            }
            Err(error) => error,
        };

        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(detail.contains("mmio auto pool is exhausted"));
        assert!(detail.contains("slot registers for auto-mmio0"));
    }

    #[test]
    fn ordinary_auto_mmio_exhausts_the_pci_low_pool_without_using_extension() {
        const ORDINARY_AUTO_MMIO_SIZE: u64 = 0x0100_0000;

        // This is a planning-layer regression rather than a provided-DTB test.
        // Retained static `reg` conflicts belong to the later FDT stage. Here
        // the ordinary device sorts after pci-host, whose ECAM and memory
        // aperture leave less than 16 MiB in the default low pool.
        let controller = DeviceNodeId::new("vgic").unwrap();
        let nodes = std::vec![
            DeviceNodeSpec::firmware_only(controller.clone()),
            DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("endpoint0").unwrap(),
                Arc::new(TestEndpointModel),
            ),
            DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("zz-auto-mmio0").unwrap(),
                Arc::new(TestAutoMmioModel {
                    size: ORDINARY_AUTO_MMIO_SIZE,
                }),
            ),
        ];
        let pools = super::super::resource_pools::create_auto_mmio_pools().unwrap();

        let plan = VmDevicePlan::with_optional_pci_host_for_vm(
            &config(true),
            nodes,
            &[],
            pools,
            provider(&controller).unwrap(),
            super::super::resource_pools::PCI_AUTO_MMIO_SEARCH_EXTENSION.clone(),
        );
        let error = match plan {
            Ok(devices) => {
                let resources = devices
                    .graph()
                    .resources_for(&DeviceNodeId::new("zz-auto-mmio0").unwrap())
                    .unwrap();
                let (base, _) = resources
                    .mmio(&ResourceSlot::new("registers").unwrap())
                    .unwrap();
                panic!("ordinary auto MMIO was allocated at {base:#x} from the PCI host extension");
            }
            Err(error) => error,
        };

        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(detail.contains("mmio auto pool is exhausted"));
        assert!(detail.contains("slot registers for zz-auto-mmio0"));
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
            &[(
                "occupied-32-bit-mmio",
                0x0c00_0000..pci_auto_mmio_search_end(),
            )],
        )
        .err()
        .expect("the PCI aperture must not move above 4 GiB");

        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(detail.contains("auto pool is exhausted"));
        assert!(detail.contains("slot memory-aperture for pci-host"));
    }

    #[test]
    fn endpoint_without_guest_dtb_is_rejected() {
        let devices = device_plan(true, &[]).unwrap();
        let error = resolve_plan(&config(false), devices.graph()).unwrap_err();
        assert!(matches!(error, AxVmError::Unsupported { .. }));
        assert!(error.to_string().contains("require a guest DTB"));
    }
}
