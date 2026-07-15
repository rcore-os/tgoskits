//! RISC-V VM resource creation and initialization.

use alloc::sync::Arc;

use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use riscv_vcpu::RiscvVcpuCreateConfig;

use super::{Riscv64Arch, irq, npt};
use crate::{
    AxVmResult, ax_err,
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
                let mut factories = default_device_factories()?;
                let mode = vm.interrupt_mode();
                let emulated_devices = vm.with_config(|config| config.emu_devices().clone());
                let interrupt_topology = irq::configure(&mut factories, mode, &emulated_devices)?;
                init_vm_with(vm, &factories, interrupt_topology)
            }
            VmInitRequest::Provided {
                factories,
                interrupt_topology,
            } => init_vm_with(vm, factories, interrupt_topology),
        }
    }
}

fn init_vm_with(
    vm: &AxVM,
    factories: &axdevice::DeviceFactoryRegistry,
    interrupt_topology: Arc<axdevice::InterruptTopology>,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_topology, |resources, interrupt_topology| {
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
        let mut devices = PreparedDevices::empty();
        devices.register_configured(
            resources.config().emu_devices(),
            factories,
            interrupt_topology,
        )?;
        devices.register_special_devices(vm)?;
        let external_irq_sources = resources.config().pass_through_irqs().to_vec();
        resources
            .arch_state_mut()
            .connect_external_irq_lines(interrupt_topology, &external_irq_sources)?;
        interrupt_topology.finalize(&vcpus.interrupt_ports(vm.id(), &placements)?)?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    _config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmRiscvVcpu as VmArchVcpuOps>::SetupConfig> {
    Ok(())
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    let mut levels = riscv_vcpu::max_guest_page_table_levels();
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        levels = levels.min(
            crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
                .unwrap_or_else(riscv_vcpu::max_guest_page_table_levels),
        );
    }
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
