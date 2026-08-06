//! RISC-V VM resource creation and initialization.

use std::sync::Arc;

use axdevice::DeviceFactoryRegistry;
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use riscv_vcpu::RiscvVcpuCreateConfig;

use super::{
    Riscv64Arch,
    irq::{self, RiscvPlicRuntime},
    npt,
};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::AxVMConfig,
    vm::{
        AxVM, AxVMResources,
        prepare::{
            PreparedVm, VmInitRequest,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init, default_device_factories,
            devices::PreparedDevices,
            validate_guest_dtb,
            vcpus::{PreparedVcpus, vcpu_placements},
        },
    },
};

impl Riscv64Arch {
    pub(crate) fn create_vm_resources(config: AxVMConfig) -> AxVmResult<AxVMResources> {
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
            nested_paging_config(root_paddr, levels)
        })
    }

    pub(crate) fn init_vm(vm: &AxVM, request: VmInitRequest<'_>) -> AxVmResult {
        match request {
            VmInitRequest::Default => {
                let mut factories = default_device_factories(vm)?;
                let runtime = register_device_factory(vm, &mut factories)?;
                init_vm_with(vm, &factories, runtime)
            }
            VmInitRequest::Provided { factories } => {
                let runtime = register_device_factory(vm, factories)?;
                init_vm_with(vm, factories, runtime)
            }
        }
    }
}

fn register_device_factory(
    vm: &AxVM,
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult<Arc<RiscvPlicRuntime>> {
    let (configs, placements, physical_irqs) = vm.with_config(|config| {
        (
            config.emu_devices().clone(),
            config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids(),
            config.pass_through_irqs().to_vec(),
        )
    });
    let physical_target_cpu = placements
        .first()
        .map(|(_, _, physical_id)| *physical_id)
        .ok_or_else(|| AxVmError::invalid_config("a RISC-V VM must contain at least one vCPU"))?;
    irq::register_device_factory(
        vm.id(),
        placements.len(),
        factories,
        &configs,
        &physical_irqs,
        physical_target_cpu,
    )
}

fn init_vm_with(
    vm: &AxVM,
    factories: &DeviceFactoryRegistry,
    runtime: Arc<RiscvPlicRuntime>,
) -> AxVmResult {
    let interrupt_controller: Arc<dyn axdevice_base::VirtualInterruptController> = runtime;
    complete_vm_init(
        vm,
        interrupt_controller,
        |resources, interrupt_controller| {
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
            let devices = PreparedDevices::build_common(
                resources,
                factories,
                interrupt_controller,
                vm.device_access_ports(),
            )?;
            validate_guest_dtb(resources)?;

            let owned_regions = guest_owned_regions(resources);
            map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
            vcpus.setup(resources, build_vcpu_setup_config)?;

            Ok(PreparedVm::new(vcpus, devices))
        },
    )
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
