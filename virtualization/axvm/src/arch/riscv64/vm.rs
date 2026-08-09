//! RISC-V VM resource creation and initialization.

use axdevice::{DeviceFirmwareBinding, DeviceNodeId, DeviceNodeSpec};
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use riscv_vcpu::RiscvVcpuCreateConfig;

use super::*;
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::*,
    vm::{
        prepare::{device_plan::*, devices::*, vcpus::*, *},
        *,
    },
};

pub(crate) type RiscvVmPlan = SimpleVmPlan;

impl Riscv64Arch {
    pub(crate) fn create_vm_resources(
        config: AxVMConfig,
        _fw_cfg_payload: std::sync::Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        let device_plan = plan_devices(&config)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, device_plan, |root_paddr| {
            nested_paging_config(root_paddr, levels)
        })
    }

    pub(crate) fn init_vm(vm: &AxVM) -> AxVmResult {
        vm.prepare_resources_with(|resources| {
            let placements = resources.vcpu_placements();
            let dtb_addr = resources
                .config()
                .image_config()
                .dtb_load_gpa
                .unwrap_or_default();
            let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
                Ok(RiscvVcpuCreateConfig {
                    hart_id: placement.phys_cpu_id,
                    dtb_addr: dtb_addr.as_usize(),
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

fn plan_devices(config: &AxVMConfig) -> AxVmResult<RiscvVmPlan> {
    let profile = config
        .plic_profile()
        .ok_or_else(|| AxVmError::invalid_config("RISC-V machine profile has no PLIC"))?;
    let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
    let physical_target_cpu = placements
        .first()
        .map(|(_, _, physical_id)| *physical_id)
        .ok_or_else(|| AxVmError::invalid_config("a RISC-V VM must contain at least one vCPU"))?;
    let controller_id = DeviceNodeId::new("plic")?;
    let controller = DeviceNodeSpec::host_replacement(
        controller_id.clone(),
        irq::model(
            config.id(),
            placements.len(),
            profile.base,
            profile.length,
            config.pass_through_irqs(),
            physical_target_cpu,
        )?,
    )
    .with_firmware_binding(DeviceFirmwareBinding::FdtNode(profile.node_path.clone()));
    let replacement_ranges =
        std::vec![profile.base as u64..profile.base as u64 + profile.length as u64,];
    let mut nodes = std::vec![controller];
    crate::configured::append_configured_devices(
        config,
        &mut nodes,
        &controller_id,
        axdevice_base::InterruptControllerId::new(0),
    )?;
    Ok(SimpleVmPlan::new(VmDevicePlan::with_pools_for_vm(
        config,
        nodes,
        &replacement_ranges,
        super::resource_pools::create(config)?,
    )?))
}

fn build_vcpu_setup_config(
    _config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmRiscvVcpu as VmArchVcpuOps>::SetupConfig> {
    Ok(())
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    let levels = crate::architecture::minimum_recorded_target_cpu_capability(
        "RISC-V G-stage page-table levels",
        vcpu_mappings,
        |cpu_id| {
            crate::percpu::select_cpu_virtualization_capability(cpu_id, |levels, _, _| {
                levels as u64
            })
        },
    )
    .map_err(|error| {
        crate::architecture::unsupported_target_cpu_capability(
            "select RISC-V target CPU capability",
            error,
        )
    })? as usize;
    match levels {
        3 | 4 => Ok(levels),
        _ => ax_err!(Unsupported, "no supported RISC-V G-stage paging mode"),
    }
}

fn nested_paging_config(
    root_paddr: ax_memory_addr::PhysAddr,
    levels: usize,
) -> AxVmResult<NestedPagingConfig> {
    match levels {
        3 => Ok(NestedPagingConfig::new(root_paddr, 3, 41, 8)),
        4 => Ok(NestedPagingConfig::new(root_paddr, 4, 50, 9)),
        _ => ax_err!(InvalidInput, "unsupported RISC-V G-stage levels"),
    }
}
