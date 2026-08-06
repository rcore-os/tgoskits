//! RISC-V VM resource creation and initialization.

use axdevice::{DeviceFirmwareBinding, DeviceNodeId, DeviceNodeSpec};
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use riscv_vcpu::RiscvVcpuCreateConfig;

use super::{Riscv64Arch, irq, npt};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::AxVMConfig,
    vm::{
        AxVM, AxVMResources,
        prepare::{
            PreparedVm,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init,
            device_plan::{SimpleVmPlan, VmDevicePlan},
            devices::PreparedDevices,
            validate_guest_dtb,
            vcpus::{PreparedVcpus, vcpu_placements},
        },
    },
};

pub(crate) type RiscvVmPlan = SimpleVmPlan;

impl Riscv64Arch {
    pub(crate) fn create_vm_resources(
        config: AxVMConfig,
        _fw_cfg_payload: alloc::sync::Arc<axdevice::FwCfgPayloadSlot>,
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
        init_vm_with(vm)
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
    let serial_id = DeviceNodeId::new("serial")?;
    let serial = if let Some(identity) = config.serial_fdt_identity() {
        DeviceNodeSpec::host_replacement(serial_id, crate::machine::serial_device_model(config))
            .with_firmware_binding(DeviceFirmwareBinding::FdtNode(identity.node_path.clone()))
    } else {
        DeviceNodeSpec::virtual_device(serial_id, crate::machine::serial_device_model(config))
    }
    .with_dependency(controller_id.clone());
    let replacement_ranges =
        alloc::vec![profile.base as u64..profile.base as u64 + profile.length as u64,];
    let mut nodes = alloc::vec![controller, serial];
    crate::configured::append_configured_devices(config, &mut nodes, &controller_id)?;
    Ok(SimpleVmPlan::new(VmDevicePlan::with_pools_for_vm(
        config,
        nodes,
        &replacement_ranges,
        super::resource_pools::create(config)?,
    )?))
}

fn init_vm_with(vm: &AxVM) -> AxVmResult {
    complete_vm_init(vm, |resources| {
        let placements = vcpu_placements(resources);
        let dtb_addr = resources
            .config()
            .image_config()
            .dtb_load_gpa
            .unwrap_or_default();
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
            Ok(RiscvVcpuCreateConfig {
                hart_id: placement.id,
                dtb_addr: dtb_addr.as_usize(),
            })
        })?;
        let devices = PreparedDevices::build_planned(resources, vm.device_access_ports())?;
        let interrupt_controller = devices
            .devices()
            .interrupt_controller(axdevice_base::InterruptControllerId::new(0))?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices, interrupt_controller))
    })
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
