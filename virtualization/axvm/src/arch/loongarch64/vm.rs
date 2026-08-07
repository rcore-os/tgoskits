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
        mut config: AxVMConfig,
        fw_cfg_payload: Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        super::boot::probe::apply_host_serial(&mut config)?;
        let device_plan = plan_devices(&config, fw_cfg_payload)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, device_plan, |root_paddr| {
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
        vm.prepare_resources_with(|resources| {
            let placements = resources.vcpu_placements();
            let state_count = placements
                .iter()
                .map(|placement| placement.id)
                .max()
                .map_or(0, |vcpu_id| vcpu_id + 1);
            let iocsr_state =
                loongarch_result(loongarch_vcpu::LoongArchIocsrState::new(state_count))
                    .map_err(|error| AxVmError::vcpu("create LoongArch IOCSR state", error))?;
            let dtb_addr = resources
                .config()
                .image_config()
                .dtb_load_gpa
                .unwrap_or_default();
            let firmware_boot = uses_firmware_boot(resources.config());
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
            resources.prepare_guest_address_space(vm.id(), &[])?;
            vcpus.setup(resources, build_vcpu_setup_config)?;

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
        firmware_boot: uses_firmware_boot(config),
    })
}

fn uses_firmware_boot(config: &AxVMConfig) -> bool {
    matches!(
        config.boot_policy(),
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: crate::config::VMBootProtocol::Uefi,
        }
    )
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
