//! LoongArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
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
            PreparedVm,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init,
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
    super::ns16550_model::register_ns16550_model(&mut registry, 0x1000)?;
    super::ns16550_model::register_dw_apb_uart_model(&mut registry, 0x1000)?;
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
        interrupt_topology.finalize(&vcpus.interrupt_ports(vm.id(), &placements)?)?;
        devices.register_planned(
            resources.config().machine_plan(),
            models,
            interrupt_topology,
            &interrupt_authority,
        )?;
        devices.register_special_devices(vm)?;
        let external_irq_sources = resources
            .config()
            .machine_plan()
            .assigned_host_interrupts()
            .to_vec();
        let physical_interrupt_policy = resources.config().physical_interrupt_policy();
        resources.arch_state_mut().connect_external_irq_lines(
            interrupt_topology,
            &interrupt_authority,
            physical_interrupt_policy,
            &external_irq_sources,
        )?;
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
    let layout = match config.machine_plan().interrupt_controller() {
        Some(crate::machine::InterruptControllerPlan::LoongArch(layout)) => layout,
        Some(_) => {
            return Err(AxVmError::invalid_config(
                "LoongArch VM machine plan contains a controller for another architecture",
            ));
        }
        None => {
            return Err(AxVmError::invalid_config(
                "LoongArch VM machine plan has no mandatory PCH-PIC/EIOINTC controller",
            ));
        }
    };
    let pch_pic_base = usize::try_from(layout.pch_pic().base())
        .map_err(|_| AxVmError::invalid_config("PCH-PIC base exceeds usize"))?;
    let pch_pic_size = usize::try_from(layout.pch_pic().size())
        .map_err(|_| AxVmError::invalid_config("PCH-PIC size exceeds usize"))?;

    let pch_pic = Arc::new(axdevice::LoongArchPchPic::new(
        pch_pic_base.into(),
        pch_pic_size,
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
        pch_pic_base, pch_pic_size
    );
    Ok(())
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    _memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmLoongArchVcpu as VmArchVcpuOps>::SetupConfig> {
    let passthrough = config.physical_interrupt_policy()
        == axvm_types::PhysicalInterruptPolicy::HardwareForwarded;
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
