//! x86_64 VM resource creation and initialization.

use alloc::sync::Arc;

#[cfg(feature = "vmx")]
use ax_memory_addr::PAGE_SIZE_4K;
use axdevice_base::{DeviceRegistry as _, PortDeviceAdapter};
#[cfg(feature = "vmx")]
use axvm_types::MappingFlags;
use axvm_types::{NestedPagingConfig, VmArchVcpuOps};
use x86_vcpu::{
    X86GuestMemoryRegion, X86GuestPhysAddr, X86HostVirtAddr, X86VCpuCreateConfig,
    X86VCpuSetupConfig,
};

#[cfg(feature = "vmx")]
use super::x86_apic_access_page_addr;
use super::{X86_64Arch, interrupt_controller::X86InterruptController, npt, x86_result};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::AxVMConfig,
    layout::GuestOwnedRegion,
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

impl X86_64Arch {
    pub(crate) fn create_vm_resources(config: AxVMConfig) -> AxVmResult<AxVMResources> {
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements);
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
            let gpa_bits = match levels {
                3 => 39,
                4 => 48,
                _ => {
                    return ax_err!(InvalidInput, "unsupported x86 nested page-table levels");
                }
            };
            Ok(NestedPagingConfig::new(root_paddr, levels, gpa_bits, 0))
        })
    }

    pub(crate) fn init_vm(vm: &AxVM) -> AxVmResult {
        let models = default_virtual_device_models()?;
        let interrupt_topology =
            Arc::new(axdevice::InterruptTopology::new(vm.interrupt_delivery()));
        init_vm_with(vm, &models, interrupt_topology)
    }
}

fn default_virtual_device_models() -> AxVmResult<axdevice::VirtualDeviceModelRegistry> {
    let mut registry = axdevice::VirtualDeviceModelRegistry::new();
    super::serial::register_standard_model(&mut registry)?;
    Ok(registry)
}

fn init_vm_with(
    vm: &AxVM,
    models: &axdevice::VirtualDeviceModelRegistry,
    interrupt_topology: Arc<axdevice::InterruptTopology>,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_topology, |resources, interrupt_topology| {
        let placements = vcpu_placements(resources);
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VCpuCreateConfig))?;
        let mut devices = PreparedDevices::empty();
        register_interrupt_controller(
            resources.config(),
            &mut devices.devices,
            interrupt_topology,
        )?;
        interrupt_topology.finalize(&vcpus.interrupt_ports(vm.id(), &placements)?)?;
        register_pit(&mut devices.devices, interrupt_topology)?;
        devices.register_planned(
            resources.config().machine_plan(),
            models,
            interrupt_topology,
        )?;
        register_planned_host_ports(resources.config().machine_plan(), &mut devices.devices)?;
        devices.register_special_devices(vm)?;
        validate_guest_dtb(resources)?;

        let mut owned_regions = guest_owned_regions(resources);
        append_arch_owned_regions(&mut owned_regions);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        map_arch_address_space(resources)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;
        super::irq::register_planned_ioapic_forwarding_routes(resources.config().machine_plan())?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmX86Vcpu as VmArchVcpuOps>::SetupConfig> {
    let mut setup_config = X86VCpuSetupConfig {
        emulate_com1: config
            .machine_plan()
            .virtual_devices()
            .iter()
            .any(|device| device.model_id().as_str() == "x86-com1"),
        guest_memory_regions: memory_regions
            .iter()
            .map(|region| X86GuestMemoryRegion {
                gpa: X86GuestPhysAddr::from_usize(region.gpa.as_usize()),
                hva: X86HostVirtAddr::from_usize(region.hva.as_usize()),
                size: region.size(),
            })
            .collect(),
        ..Default::default()
    };
    for port in config.machine_plan().assigned_host_pio() {
        x86_result(setup_config.add_passthrough_port_range(port.base(), port.size()))
            .map_err(|error| AxVmError::vcpu("configure passthrough port range", error))?;
    }
    Ok(setup_config)
}

fn register_interrupt_controller(
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
    interrupt_topology: &axdevice::InterruptTopology,
) -> AxVmResult {
    let layout = match config.machine_plan().interrupt_controller() {
        Some(crate::machine::InterruptControllerPlan::X86Apic(layout)) => layout,
        Some(_) => {
            return Err(AxVmError::invalid_config(
                "x86 VM machine plan contains a controller for another architecture",
            ));
        }
        None => {
            return Err(AxVmError::invalid_config(
                "x86 VM machine plan has no mandatory APIC controller",
            ));
        }
    };
    let ioapic_base = usize::try_from(layout.ioapic().base())
        .map_err(|_| AxVmError::invalid_config("IOAPIC base exceeds usize"))?;
    let ioapic_size = usize::try_from(layout.ioapic().size())
        .map_err(|_| AxVmError::invalid_config("IOAPIC size exceeds usize"))?;

    let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
        x86_vlapic::X86GuestPhysAddr::from_usize(ioapic_base),
        Some(ioapic_size),
    ));
    let controller = Arc::new(X86InterruptController::new(
        axdevice::InterruptControllerId::new(0),
        ioapic.clone(),
    ));
    devices
        .add_x86_ioapic_controller(
            ioapic,
            controller.clone(),
            controller.registration(),
            interrupt_topology,
        )
        .map_err(|error| AxVmError::device("register x86 APIC topology", error))?;
    info!(
        "x86 IOAPIC initialized with base GPA {:#x} and length {:#x}",
        ioapic_base, ioapic_size
    );
    Ok(())
}

fn register_pit(
    devices: &mut axdevice::AxVmDevices,
    interrupt_topology: &axdevice::InterruptTopology,
) -> AxVmResult {
    let irq = interrupt_topology
        .connect_irq(axdevice::WiredIrqRequest::new(
            axdevice::ControllerInputId::new(super::irq::PIT_TIMER_GSI),
            axvm_types::InterruptTriggerMode::EdgeTriggered,
        ))
        .map_err(|error| AxVmError::device("connect x86 PIT IRQ0", error))?;
    let pit = Arc::new(axdevice::X86PitDevice::<super::AxvmX86HostOps>::new_with_irq(irq));
    devices
        .add_x86_pit_dev(pit)
        .map_err(|error| AxVmError::device("register x86 PIT", error))?;
    info!("x86 PIT initialized for ports 0x40..=0x43 and 0x61");
    Ok(())
}

fn register_planned_host_ports(
    plan: &crate::machine::VmMachinePlan,
    devices: &mut axdevice::AxVmDevices,
) -> AxVmResult {
    for range in plan.assigned_host_pio() {
        let passthrough = Arc::new(super::port::HostPortPassthrough::new(
            range.base(),
            range.size(),
        )?);
        devices
            .register(PortDeviceAdapter::from_arc(passthrough))
            .map_err(|error| AxVmError::device("register planned host PIO range", error))?;
    }
    Ok(())
}

fn append_arch_owned_regions(regions: &mut alloc::vec::Vec<GuestOwnedRegion>) {
    #[cfg(feature = "vmx")]
    regions.push(GuestOwnedRegion::new(
        x86_vcpu::X86_APIC_ACCESS_GPA,
        PAGE_SIZE_4K,
        crate::layout::VmRegionKind::Reserved,
    ));
    #[cfg(not(feature = "vmx"))]
    let _ = regions;
}

fn map_arch_address_space(resources: &mut AxVMResources) -> AxVmResult {
    #[cfg(feature = "vmx")]
    resources
        .address_space
        .map_linear(
            axvm_types::GuestPhysAddr::from(x86_vcpu::X86_APIC_ACCESS_GPA),
            x86_apic_access_page_addr(),
            PAGE_SIZE_4K,
            MappingFlags::DEVICE | MappingFlags::READ | MappingFlags::WRITE,
        )
        .map_err(|error| AxVmError::memory("map x86 APIC access page", error))?;
    #[cfg(not(feature = "vmx"))]
    let _ = resources;
    Ok(())
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> usize {
    let mut levels = 4;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        levels = levels.min(crate::percpu::cpu_max_guest_page_table_levels(cpu_id).unwrap_or(4));
    }
    levels
}
