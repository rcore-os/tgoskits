//! LoongArch64 VM resource creation and initialization.

use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use loongarch_vcpu::{LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig};

use super::{LoongArch64Arch, loongarch_result, npt};
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

impl LoongArch64Arch {
    pub(crate) fn create_vm_resources(config: AxVMConfig) -> AxVmResult<AxVMResources> {
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements);
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
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

    pub(crate) fn init_vm(vm: &AxVM, request: VmInitRequest<'_>) -> AxVmResult {
        match request {
            VmInitRequest::Default => {
                let factories = default_device_factories()?;
                let interrupt_fabric = crate::InterruptFabric::new(vm.interrupt_mode());
                init_vm_with(vm, &factories, interrupt_fabric)
            }
            VmInitRequest::Provided {
                factories,
                interrupt_fabric,
            } => init_vm_with(vm, factories, interrupt_fabric),
        }
    }
}

fn init_vm_with(
    vm: &AxVM,
    factories: &axdevice::DeviceFactoryRegistry,
    interrupt_fabric: crate::InterruptFabric,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_fabric, |resources, interrupt_fabric| {
        let placements = vcpu_placements(resources);
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
        let mut devices = PreparedDevices::build_common(resources, factories, interrupt_fabric)?;
        devices.register_special_devices(vm)?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmLoongArchVcpu as VmArchVcpuOps>::SetupConfig> {
    let passthrough = config.interrupt_mode() == axvm_types::VMInterruptMode::Passthrough;
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

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> usize {
    let mut levels = 4;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        levels = levels.min(crate::percpu::cpu_max_guest_page_table_levels(cpu_id).unwrap_or(4));
    }
    levels
}
