//! LoongArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use axvm_types::{EmulatedDeviceType, NestedPagingConfig, VmArchVcpuOps};
use loongarch_vcpu::{LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig};

use super::{
    LoongArch64Arch, interrupt_controller::LoongArchInterruptController, loongarch_result, npt,
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
                let interrupt_topology =
                    Arc::new(axdevice::InterruptTopology::new(vm.interrupt_mode()));
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
        let mut devices = PreparedDevices::empty();
        register_interrupt_controller(
            resources.config(),
            &mut devices.devices,
            interrupt_topology,
        )?;
        devices.register_configured(
            resources.config().emu_devices(),
            factories,
            interrupt_topology,
        )?;
        devices.register_special_devices(vm)?;
        interrupt_topology.finalize(&vcpus.interrupt_ports(vm.id(), &placements)?)?;
        validate_guest_dtb(resources)?;

        let owned_regions = guest_owned_regions(resources);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn register_interrupt_controller(
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
    interrupt_topology: &axdevice::InterruptTopology,
) -> AxVmResult {
    let mut pch_pic_configs = config
        .emu_devices()
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::LoongArchPchPic);
    let Some(pch_pic_config) = pch_pic_configs.next() else {
        return Ok(());
    };
    if pch_pic_configs.next().is_some() {
        return Err(AxVmError::invalid_config(
            "a LoongArch VM may register only one PCH-PIC controller",
        ));
    }

    let pch_pic = Arc::new(axdevice::LoongArchPchPic::new(
        pch_pic_config.base_gpa.into(),
        pch_pic_config.length,
    ));
    let controller = Arc::new(LoongArchInterruptController::new(
        axdevice::InterruptControllerId::new(0),
        pch_pic.clone(),
    ));
    devices
        .add_loongarch_pch_pic_controller(
            axdevice::MmioDeviceAdapter::from_arc(pch_pic),
            controller.clone(),
            controller.registration(),
            interrupt_topology,
        )
        .map_err(|error| AxVmError::device("register LoongArch PCH-PIC/EIOINTC topology", error))?;
    info!(
        "LoongArch PCH-PIC initialized with base GPA {:#x} and length {:#x}",
        pch_pic_config.base_gpa, pch_pic_config.length
    );
    Ok(())
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
