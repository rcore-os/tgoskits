//! AArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use arm_vcpu::{ArmVcpuCreateConfig, ArmVcpuSetupConfig};
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};

use super::{
    Aarch64Arch,
    gic::{PreparedGicV3, register_maintenance_interrupt},
    npt, timer,
};
use crate::{
    AxVmResult, ax_err,
    config::AxVMConfig,
    vm::{
        AxVM, AxVMResources,
        prepare::{
            PreparedVm,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init,
            devices::PreparedDevices,
            validate_guest_dtb,
            vcpus::{PreparedVcpus, vcpu_placements},
        },
    },
};

impl Aarch64Arch {
    pub(crate) fn create_vm_resources(mut config: AxVMConfig) -> AxVmResult<AxVMResources> {
        super::placement::normalize_hardware_forwarded_vcpu_cpu_sets(&mut config)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
            nested_paging_config(root_paddr, levels, &placements)
        })
    }

    pub(crate) fn init_vm(vm: &AxVM) -> AxVmResult {
        let models = default_virtual_device_models()?;
        let (interrupt_topology, interrupt_authority) = axdevice::InterruptTopology::new();
        init_vm_with(
            vm,
            &models,
            Arc::new(interrupt_topology),
            interrupt_authority,
        )
    }
}

fn default_virtual_device_models() -> AxVmResult<axdevice::VirtualDeviceModelRegistry> {
    let mut registry = axdevice::VirtualDeviceModelRegistry::new();
    super::pl011::register_standard_model(&mut registry)?;
    Ok(registry)
}

fn init_vm_with(
    vm: &AxVM,
    models: &axdevice::VirtualDeviceModelRegistry,
    interrupt_topology: Arc<axdevice::InterruptTopology>,
    interrupt_authority: axdevice::InterruptPlanAuthority,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_topology, |resources, interrupt_topology| {
        let placements = vcpu_placements(resources);
        let dtb_addr = resources
            .config()
            .image_config()
            .dtb_load_gpa
            .unwrap_or_default();
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
            Ok(ArmVcpuCreateConfig {
                mpidr_el1: placement.phys_cpu_id as _,
                dtb_addr: dtb_addr.as_usize(),
            })
        })?;
        let prepared_gic = PreparedGicV3::from_vm_config(vm.id(), resources.config(), &placements)?;
        let mut devices = PreparedDevices::empty();
        prepared_gic.register(&mut devices.devices, interrupt_topology)?;
        let ports = vcpus.interrupt_ports(vm.id(), &placements)?;
        interrupt_topology.finalize(&ports)?;
        let interrupt_roles = resources.config().arch().interrupt_roles().ok_or_else(|| {
            crate::AxVmError::invalid_config("AArch64 interrupt roles are not prepared")
        })?;
        let maintenance_interrupt = register_maintenance_interrupt(interrupt_roles, &placements)?;
        let host_spi_forwarding =
            prepared_gic.connect_physical_spis(interrupt_topology, &interrupt_authority)?;
        let physical_timer_ppi = resources
            .config()
            .arch()
            .interrupt_roles()
            .map(super::gic::Aarch64InterruptRoles::guest_physical_timer);
        timer::register_emulated_timers(
            &mut devices,
            prepared_gic.device_set(),
            &placements,
            interrupt_topology,
            &interrupt_authority,
            physical_timer_ppi,
        )?;
        devices.register_planned(
            resources.config().machine_plan(),
            models,
            interrupt_topology,
            &interrupt_authority,
        )?;
        devices.register_special_devices(vm)?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;
        resources.arch_state_mut().set_gic_controller(
            prepared_gic.controller(),
            host_spi_forwarding,
            maintenance_interrupt,
        );

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    _config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmArmVcpu as VmArchVcpuOps>::SetupConfig> {
    Ok(ArmVcpuSetupConfig)
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    let mut selected = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let levels = crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
            .unwrap_or_else(arm_vcpu::max_guest_page_table_levels);
        if levels == 0 {
            return ax_err!(
                Unsupported,
                "AArch64 nested paging is not enabled on target CPU"
            );
        }
        selected = selected.min(levels);
    }
    if selected == usize::MAX {
        selected = arm_vcpu::max_guest_page_table_levels();
    }
    match selected {
        3 | 4 => Ok(selected),
        _ => ax_err!(Unsupported, "unsupported AArch64 stage-2 page-table levels"),
    }
}

fn nested_paging_config(
    root_paddr: ax_memory_addr::PhysAddr,
    levels: usize,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
) -> AxVmResult<NestedPagingConfig> {
    let mut pa_bits = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let bits =
            crate::percpu::cpu_guest_phys_addr_bits(cpu_id).unwrap_or_else(arm_vcpu::pa_bits);
        pa_bits = pa_bits.min(bits);
    }
    if pa_bits == usize::MAX {
        pa_bits = arm_vcpu::pa_bits();
    }

    let gpa_bits = match levels {
        3 => 39,
        4 => 48,
        _ => return ax_err!(InvalidInput, "unsupported AArch64 stage-2 levels"),
    };
    Ok(NestedPagingConfig::new(
        root_paddr, levels, gpa_bits, pa_bits,
    ))
}
