//! LoongArch machine model for the guest PCI ECAM aperture.

use std::{sync::Arc, vec::Vec};

use axdevice::*;

use super::boot::PciHost;
use crate::AxVmResult;

const ECAM_SLOT: &str = "registers";
const ECAM_ALIGNMENT: u64 = 1 << 20;

pub(super) fn append_pci_ecam_node(
    profile: Option<PciHost>,
    nodes: &mut Vec<DeviceNodeSpec>,
) -> AxVmResult {
    let Some(profile) = profile else {
        return Ok(());
    };
    nodes.push(DeviceNodeSpec::virtual_device(
        DeviceNodeId::new("pci-ecam")?,
        Arc::new(LoongArchPciEcamModel::new(profile)?),
    ));
    Ok(())
}

#[derive(Debug)]
struct LoongArchPciEcamModel {
    profile: PciHost,
}

impl LoongArchPciEcamModel {
    fn new(profile: PciHost) -> DeviceManagerResult<Self> {
        create_pci_ecam_device(
            (profile.ecam.base, profile.ecam.size),
            "create LoongArch PCI ECAM model",
        )?;
        Ok(Self { profile })
    }
}

impl DeviceModel for LoongArchPciEcamModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new(ECAM_SLOT)?,
            self.profile.ecam.size,
            ECAM_ALIGNMENT,
            ResourceRequest::Fixed(self.profile.ecam.base),
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            None,
            Some(std::vec![AcpiContributionSpec::PciHostBridge(
                AcpiDeviceSpec::new("PCI0", "PNP0A08").with_register(
                    ResourceSlot::new(ECAM_SLOT).expect("static PCI ECAM slot is valid"),
                ),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.mmio(ECAM_SLOT)?;
        let expected = (self.profile.ecam.base, self.profile.ecam.size);
        if range != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build LoongArch PCI ECAM device",
                detail: std::format!(
                    "planned ECAM range {:#x}..{:#x} differs from normalized profile {:#x}..{:#x}",
                    range.0,
                    range.0.saturating_add(range.1),
                    expected.0,
                    expected.0.saturating_add(expected.1)
                ),
            });
        }
        let device = create_pci_ecam_device(range, "build LoongArch PCI ECAM device")?;
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(device),
        )))
    }
}

fn create_pci_ecam_device(
    range: (u64, u64),
    operation: &'static str,
) -> DeviceManagerResult<PciEcamDevice> {
    PciEcamDevice::new(range.0, range.1).map_err(|error| DeviceManagerError::InvalidConfig {
        operation,
        detail: std::format!(
            "invalid normalized PCI ECAM aperture at base {:#x}, size {:#x}: {error}",
            range.0,
            range.1
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ax_std::os::arceos::driver::probe::acpi::AcpiPciEcam;
    use axdevice::{
        AcpiContributionSpec, DeviceGraphBuilder, DeviceManagerError, DeviceModel, DeviceNodeId,
        DeviceNodeSpec, DeviceRequirement, DeviceRuntimeBuilder, ResourcePools, ResourceRequest,
        ResourceSlot, RuntimeAccessPorts,
    };
    use axdevice_base::Resource;

    use super::*;
    use crate::{
        AxVmError,
        arch::loongarch64::boot::{MmioRegion, PciHost, probe},
        config::AxVMConfig,
        vm::prepare::device_plan::VmDevicePlan,
    };

    const ECAM_BASE: u64 = 0x2000_0000;
    const ECAM_SIZE: u64 = 0x0800_0000;

    #[test]
    fn model_requests_the_normalized_fixed_ecam_window() {
        let model = LoongArchPciEcamModel::new(pci_profile()).unwrap();

        let requirements = model.requirements().unwrap();

        assert_eq!(
            requirements.entries(),
            &[DeviceRequirement::Mmio {
                slot: ResourceSlot::new("registers").unwrap(),
                size: ECAM_SIZE,
                alignment: 0x0010_0000,
                request: ResourceRequest::Fixed(ECAM_BASE),
            }]
        );
    }

    #[test]
    fn firmware_is_acpi_only_pcie_host_bridge_using_the_ecam_slot() {
        let firmware = LoongArchPciEcamModel::new(pci_profile())
            .unwrap()
            .firmware();

        assert!(firmware.fdt().is_none());
        let Some([AcpiContributionSpec::PciHostBridge(device)]) = firmware.acpi() else {
            panic!("expected exactly one ACPI PCI host bridge");
        };
        assert_eq!(device.name(), "PCI0");
        assert_eq!(device.hid(), Some("PNP0A08"));
        assert_eq!(
            device.register_slots(),
            &[ResourceSlot::new("registers").unwrap()]
        );
    }

    #[test]
    fn resolved_graph_builds_one_pci_ecam_device_at_the_planned_window() {
        let mut graph = DeviceGraphBuilder::new();
        graph
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("pci-ecam").unwrap(),
                Arc::new(LoongArchPciEcamModel::new(pci_profile()).unwrap()),
            ))
            .unwrap();
        let mut pools = ResourcePools::new();
        pools
            .allow_fixed_mmio(ECAM_BASE..ECAM_BASE + ECAM_SIZE)
            .unwrap();
        let graph = graph.declare().unwrap().resolve(pools).unwrap();
        let mut runtime = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
        for node in graph.nodes() {
            runtime
                .build_graph_node(node, graph.resource_plan())
                .unwrap();
        }

        let runtime = runtime.finish(graph.resource_plan()).unwrap();

        assert_eq!(runtime.device_count(), 1);
        let device = runtime.devices().next().unwrap();
        assert_eq!(device.name(), "pci-ecam");
        assert_eq!(
            device.resources(),
            &[Resource::MmioRange {
                base: ECAM_BASE,
                size: ECAM_SIZE,
            }]
        );
    }

    #[test]
    fn normalized_profile_prefers_host_acpi_and_defaults_only_without_ecam() {
        let host = PciHost {
            ecam: MmioRegion {
                base: 0x3000_0000,
                size: 0x0400_0000,
            },
            ..pci_profile()
        };

        assert_eq!(
            probe::normalize_guest_pci_profile(Some(Ok(Some(host)))).unwrap(),
            host
        );
        assert_eq!(
            probe::normalize_guest_pci_profile(Some(Ok(None))).unwrap(),
            probe::qemu_guest_pci_profile()
        );
        assert_eq!(
            probe::normalize_guest_pci_profile(None).unwrap(),
            probe::qemu_guest_pci_profile()
        );
    }

    #[test]
    fn normalized_profile_propagates_malformed_host_acpi() {
        let result = probe::normalize_guest_pci_profile(Some(Err(AxVmError::invalid_config(
            "malformed host ACPI MCFG",
        ))));

        let error = result.expect_err("malformed host ACPI must not fall back to QEMU defaults");
        assert!(error.to_string().contains("malformed host ACPI MCFG"));
    }

    #[test]
    fn mcfg_selection_finds_the_supported_region_regardless_of_order() {
        let supported = acpi_ecam(0, 0, 0x7f, ECAM_BASE);
        let unsupported = acpi_ecam(2, 0, 0x3f, 0x3000_0000);

        assert_eq!(
            probe::select_host_pci_ecam(&[unsupported, supported]).unwrap(),
            Some(supported)
        );
        assert_eq!(
            probe::select_host_pci_ecam(&[supported, unsupported]).unwrap(),
            Some(supported)
        );
    }

    #[test]
    fn mcfg_selection_rejects_duplicate_supported_regions() {
        let error = probe::select_host_pci_ecam(&[
            acpi_ecam(0, 0, 0x3f, ECAM_BASE),
            acpi_ecam(0, 0, 0x7f, 0x3000_0000),
        ])
        .expect_err("multiple segment-zero bus-zero regions are ambiguous");

        assert!(
            error
                .to_string()
                .contains("multiple supported host ACPI PCI ECAM regions")
        );
    }

    #[test]
    fn mcfg_selection_rejects_nonempty_unsupported_segments() {
        let error = probe::select_host_pci_ecam(&[acpi_ecam(3, 0, 0x7f, ECAM_BASE)])
            .expect_err("a nonempty unsupported MCFG must not use QEMU defaults");

        assert!(
            error
                .to_string()
                .contains("no supported segment 0 bus 0 region")
        );
        assert!(error.to_string().contains("segment 3"));
    }

    #[test]
    fn mcfg_selection_rejects_descending_bus_ranges() {
        let error = probe::select_host_pci_ecam(&[acpi_ecam(0, 4, 3, ECAM_BASE)])
            .expect_err("a descending MCFG bus range is malformed");

        assert!(error.to_string().contains("descending bus range 4..3"));
    }

    #[test]
    fn mcfg_selection_rejects_nonzero_bus_start_without_a_supported_region() {
        let error = probe::select_host_pci_ecam(&[acpi_ecam(0, 1, 0x7f, ECAM_BASE)])
            .expect_err("a nonzero bus start is unsupported by the guest ECAM model");

        assert!(
            error
                .to_string()
                .contains("no supported segment 0 bus 0 region")
        );
        assert!(error.to_string().contains("bus 1"));
    }

    #[test]
    fn empty_mcfg_selection_uses_the_qemu_fallback() {
        assert_eq!(probe::select_host_pci_ecam(&[]).unwrap(), None);
        assert_eq!(
            probe::normalize_guest_pci_profile(Some(Ok(None))).unwrap(),
            probe::qemu_guest_pci_profile()
        );
    }

    #[test]
    fn guest_platform_builder_uses_the_fallible_normalized_pci_profile() {
        let platform = probe::GuestPlatformBuilder::new(
            Vec::new(),
            None,
            crate::arch::loongarch64::boot::GuestFirmwareSelection::Uefi,
        )
        .build()
        .unwrap();

        assert_eq!(platform.pci, probe::qemu_guest_pci_profile());
    }

    #[test]
    fn planner_rejects_ecam_overlapping_guest_ram() {
        let mut config = AxVMConfig::default_for_test(1, "loongarch-ecam-ram-conflict");
        config.set_memory_regions(vec![axvm_types::VmMemConfig {
            gpa: ECAM_BASE as usize,
            size: ECAM_SIZE as usize,
            flags: 0x7,
            map_type: axvm_types::VmMemMappingType::MapIdentical,
        }]);

        let error = plan_ecam(&config, Vec::new(), ResourcePools::new())
            .err()
            .expect("guest RAM must conflict with the fixed ECAM aperture");

        assert_planning_conflict(error, "guest-memory-0", "pci-ecam");
    }

    #[test]
    fn planner_rejects_ecam_overlapping_a_reserved_machine_range() {
        let config = AxVMConfig::default_for_test(1, "loongarch-ecam-reserved-conflict");
        let mut pools = ResourcePools::new();
        pools
            .reserve_mmio(
                "loongarch-machine-reserved",
                ECAM_BASE + 0x0010_0000..ECAM_BASE + 0x0020_0000,
            )
            .unwrap();

        let error = plan_ecam(&config, Vec::new(), pools)
            .err()
            .expect("machine-reserved MMIO must conflict with ECAM");

        assert_planning_conflict(error, "loongarch-machine-reserved", "pci-ecam");
    }

    #[test]
    fn planner_rejects_ecam_overlapping_another_fixed_mmio_device() {
        let config = AxVMConfig::default_for_test(1, "loongarch-ecam-device-conflict");
        let occupant = DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("fixed-mmio-occupant").unwrap(),
            Arc::new(FixedMmioModel {
                base: ECAM_BASE + 0x0010_0000,
                size: 0x0010_0000,
            }),
        );

        let error = plan_ecam(&config, vec![occupant], ResourcePools::new())
            .err()
            .expect("another fixed MMIO device must conflict with ECAM");

        assert_planning_conflict(error, "fixed-mmio-occupant", "pci-ecam");
    }

    #[test]
    fn model_rejects_an_invalid_normalized_ecam_window() {
        let invalid = PciHost {
            ecam: MmioRegion {
                base: ECAM_BASE + 0x1000,
                size: ECAM_SIZE,
            },
            ..pci_profile()
        };

        let error = LoongArchPciEcamModel::new(invalid).unwrap_err();

        let DeviceManagerError::InvalidConfig { operation, detail } = error else {
            panic!("unexpected model error: {error:?}");
        };
        assert_eq!(operation, "create LoongArch PCI ECAM model");
        assert!(detail.contains("0x20001000"), "{detail}");
        assert!(detail.contains("0x8000000"), "{detail}");
    }

    fn plan_ecam(
        config: &AxVMConfig,
        mut nodes: Vec<DeviceNodeSpec>,
        pools: ResourcePools,
    ) -> crate::AxVmResult<VmDevicePlan> {
        nodes.push(DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("pci-ecam").unwrap(),
            Arc::new(LoongArchPciEcamModel::new(pci_profile()).unwrap()),
        ));
        VmDevicePlan::with_pools_for_vm(config, nodes, &[], pools)
    }

    fn assert_planning_conflict(error: AxVmError, first_owner: &str, second_owner: &str) {
        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected planner error: {error:?}");
        };
        assert!(detail.contains("address ranges overlap"), "{detail}");
        assert!(detail.contains(first_owner), "{detail}");
        assert!(detail.contains(second_owner), "{detail}");
    }

    struct FixedMmioModel {
        base: u64,
        size: u64,
    }

    impl DeviceModel for FixedMmioModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new().with_mmio(
                ResourceSlot::new("registers")?,
                self.size,
                0x1000,
                ResourceRequest::Fixed(self.base),
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

    fn acpi_ecam(segment_group: u16, bus_start: u8, bus_end: u8, base_address: u64) -> AcpiPciEcam {
        AcpiPciEcam {
            segment_group,
            bus_start,
            bus_end,
            base_address,
            dma_coherent: None,
        }
    }

    fn pci_profile() -> PciHost {
        PciHost {
            ecam: MmioRegion {
                base: ECAM_BASE,
                size: ECAM_SIZE,
            },
            mmio: MmioRegion {
                base: 0x4000_0000,
                size: 0x4000_0000,
            },
            io_base: 0x1800_0000,
            io_size: 0x0001_0000,
            intx_base: 16,
        }
    }
}
