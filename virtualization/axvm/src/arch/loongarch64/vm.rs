//! LoongArch64 VM resource creation and initialization.

use std::sync::Arc;

use axdevice::{DeviceNodeId, DeviceNodeSpec};
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use loongarch_vcpu::{LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig};

use super::*;
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::*,
    vm::{
        prepare::{device_plan::*, devices::*, vcpus::*, *},
        *,
    },
};

pub(crate) type LoongArchVmPlan = SimpleVmPlan;

impl LoongArch64Arch {
    pub(crate) fn create_vm_resources(
        config: &mut AxVMConfig,
        fw_cfg_payload: Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        super::boot::probe::apply_host_serial(config)?;
        let device_plan = plan_devices(config, fw_cfg_payload)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config.id(), page_table, device_plan, |root_paddr| {
            let gpa_bits = match levels {
                3 => 39,
                4 => 48,
                _ => {
                    return ax_err!(
                        InvalidInput,
                        "unsupported LoongArch nested page-table levels"
                    );
                }
            };
            Ok(NestedPagingConfig::new(root_paddr, levels, gpa_bits, 0))
        })
    }

    pub(crate) fn init_vm(vm: &AxVM) -> AxVmResult {
        vm.prepare_resources_with(|resources, config| {
            let placements = resources.vcpu_placements(config);
            let state_count = placements
                .iter()
                .map(|placement| placement.id)
                .max()
                .map_or(0, |vcpu_id| vcpu_id + 1);
            let iocsr_state =
                loongarch_result(loongarch_vcpu::LoongArchIocsrState::new(state_count))
                    .map_err(|error| AxVmError::vcpu("create LoongArch IOCSR state", error))?;
            let dtb_addr = config.image_config().dtb_load_gpa.unwrap_or_default();
            let firmware_boot = uses_firmware_boot(config)?;
            let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
                Ok(LoongArchVCpuCreateConfig {
                    cpu_id: placement.id,
                    dtb_addr: dtb_addr.as_usize(),
                    boot_args: [0; 3],
                    boot_stack_top: 0,
                    firmware_boot,
                    iocsr_state: iocsr_state.clone(),
                })
            })?;
            let devices = PreparedDevices::build_planned(resources, vm.device_access_ports())?;
            let interrupt_controller = devices
                .devices()
                .interrupt_controller(axdevice_base::InterruptControllerId::new(0))?;
            resources.prepare_guest_address_space(vm.id(), config, &[])?;
            vcpus.setup(resources, config, build_vcpu_setup_config)?;

            Ok(PreparedVm::new(vcpus, devices, interrupt_controller))
        })
    }
}

fn plan_devices(
    config: &AxVMConfig,
    fw_cfg_payload: Arc<axdevice::FwCfgPayloadSlot>,
) -> AxVmResult<LoongArchVmPlan> {
    const PCH_PIC_BASE: usize = 0x1000_0000;
    const PCH_PIC_SIZE: usize = 0x1000;
    const FW_CFG_BASE: usize = 0x1e02_0000;
    const FW_CFG_SIZE: usize = 0x18;
    let controller_id = DeviceNodeId::new("pch-pic")?;
    let mut nodes = std::vec![
        DeviceNodeSpec::host_replacement(
            controller_id.clone(),
            Arc::new(axdevice::LoongArchPchPicFactory::new(
                PCH_PIC_BASE,
                PCH_PIC_SIZE,
                Arc::new(LoongArchDomainFactory { vm_id: config.id() }),
                Arc::new(super::irq::LoongArchPchPicOutputSink::new(config.id())),
            )),
        ),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("fw-cfg")?,
            Arc::new(axdevice::FwCfgPayloadFactory::deferred(
                axvm_types::GuestPhysAddr::from(FW_CFG_BASE),
                FW_CFG_SIZE,
                fw_cfg_payload,
            )),
        ),
    ];
    let pci_profile = match super::boot::select_guest_firmware(config)? {
        super::boot::GuestFirmwareSelection::Uefi => {
            Some(super::boot::probe::normalized_guest_pci_profile()?)
        }
        super::boot::GuestFirmwareSelection::DirectFdt => None,
    };
    super::pci_ecam::append_pci_ecam_node(pci_profile, &mut nodes)?;
    crate::configured::append_configured_devices(
        config,
        &mut nodes,
        &controller_id,
        axdevice_base::InterruptControllerId::new(0),
    )?;
    Ok(SimpleVmPlan::new(VmDevicePlan::with_pools_for_vm(
        config,
        nodes,
        &[PCH_PIC_BASE as u64..(PCH_PIC_BASE + PCH_PIC_SIZE) as u64],
        super::resource_pools::create()?,
    )?))
}

#[cfg(test)]
pub(super) fn plan_devices_for_test(config: &AxVMConfig) -> AxVmResult<LoongArchVmPlan> {
    plan_devices(config, Arc::new(axdevice::FwCfgPayloadSlot::new()))
}

struct LoongArchDomainFactory {
    vm_id: usize,
}

impl axdevice::LoongArchInterruptDomainFactory for LoongArchDomainFactory {
    fn create(
        &self,
        pic: Arc<axdevice::LoongArchPchPic>,
    ) -> Arc<dyn axdevice_base::VirtualInterruptController> {
        irq::create_interrupt_domain(self.vm_id, pic)
    }
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmLoongArchVcpu as VmArchVcpuOps>::SetupConfig> {
    let passthrough = config.uses_passthrough_address_space();
    Ok(LoongArchVCpuSetupConfig {
        passthrough_interrupt: passthrough,
        passthrough_timer: passthrough,
        boot_args: [0; 3],
        boot_stack_top: 0,
        firmware_boot: uses_firmware_boot(config)?,
    })
}

fn uses_firmware_boot(config: &AxVMConfig) -> AxVmResult<bool> {
    Ok(matches!(
        super::boot::select_guest_firmware(config)?,
        super::boot::GuestFirmwareSelection::Uefi
    ))
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    crate::architecture::minimum_recorded_target_cpu_capability(
        "LoongArch nested page-table levels",
        vcpu_mappings,
        |cpu_id| {
            crate::percpu::select_cpu_virtualization_capability(cpu_id, |levels, _, _| {
                levels as u64
            })
        },
    )
    .map(|levels| levels as usize)
    .map_err(|error| {
        crate::architecture::unsupported_target_cpu_capability(
            "select LoongArch target CPU capability",
            error,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_uefi_plan_contains_exactly_one_pci_ecam_node() {
        let config = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        });

        let plan = actual_plan(&config).unwrap();

        assert_eq!(pci_ecam_node_count(&plan), 1);
    }

    #[test]
    fn actual_non_uefi_plans_do_not_contain_pci_ecam_node() {
        for policy in [
            GuestBootPolicy::AdjustKernelForBootProtocol {
                protocol: VMBootProtocol::Direct,
            },
            GuestBootPolicy::KeepConfigured,
        ] {
            let config = config_with_boot_policy(policy);
            let plan = actual_plan(&config).unwrap();

            assert_eq!(pci_ecam_node_count(&plan), 0, "policy {policy:?}");
        }
    }

    #[test]
    fn actual_multiboot_plan_is_rejected() {
        let config = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Multiboot,
        });

        let Err(error) = actual_plan(&config) else {
            panic!("LoongArch Multiboot must be unsupported");
        };

        assert!(error.to_string().contains("Multiboot"));
    }

    #[test]
    fn actual_uefi_plan_rejects_pci_ecam_overlapping_guest_memory() {
        let mut config = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        });
        config.set_memory_regions(std::vec![axvm_types::VmMemConfig {
            gpa: 0x2000_0000,
            size: 0x0800_0000,
            flags: 0x7,
            map_type: axvm_types::VmMemMappingType::MapIdentical,
        }]);

        let error = actual_plan(&config)
            .err()
            .expect("the actual plan must reject guest RAM overlapping ECAM");

        let AxVmError::Device { detail, .. } = error else {
            panic!("unexpected plan error: {error:?}");
        };
        assert!(detail.contains("guest-memory-0"), "{detail}");
        assert!(detail.contains("pci-ecam"), "{detail}");
    }

    fn config_with_boot_policy(policy: GuestBootPolicy) -> AxVMConfig {
        let mut catalog = crate::ConfiguredDeviceCatalog::new();
        crate::machine::register_devices(&mut catalog).unwrap();
        AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: "loongarch-real-plan-test".into(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            boot_policy: policy,
            virtual_device_catalog: Arc::new(catalog),
            ..Default::default()
        })
    }

    fn actual_plan(config: &AxVMConfig) -> AxVmResult<LoongArchVmPlan> {
        plan_devices(config, Arc::new(axdevice::FwCfgPayloadSlot::new()))
    }

    fn pci_ecam_node_count(plan: &LoongArchVmPlan) -> usize {
        plan.devices()
            .graph()
            .nodes()
            .filter(|node| node.id().as_str() == "pci-ecam")
            .count()
    }
}
