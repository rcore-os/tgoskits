//! x86_64 VM resource creation and initialization.

use alloc::sync::Arc;

#[cfg(feature = "vmx")]
use ax_memory_addr::PAGE_SIZE_4K;
use axdevice_base::{BaseDeviceOps, DeviceRegistry as _, PortDeviceAdapter};
#[cfg(feature = "vmx")]
use axvm_types::MappingFlags;
use axvm_types::{EmulatedDeviceType, NestedPagingConfig, VmArchVcpuOps};
use x86_vcpu::{
    X86GuestMemoryRegion, X86GuestPhysAddr, X86HostVirtAddr, X86VCpuCreateConfig,
    X86VCpuSetupConfig,
};

#[cfg(feature = "vmx")]
use super::x86_apic_access_page_addr;
use super::{X86_64Arch, interrupt_controller::X86InterruptController, npt, x86_result};
use crate::{
    AxVmError, AxVmResult, ax_err, ax_err_type,
    config::AxVMConfig,
    layout::GuestOwnedRegion,
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
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VCpuCreateConfig))?;
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
        register_arch_devices(resources.config(), &mut devices.devices, interrupt_topology)?;
        devices.register_special_devices(vm)?;
        interrupt_topology.finalize(&vcpus.interrupt_ports(vm.id(), &placements)?)?;
        validate_guest_dtb(resources)?;

        let mut owned_regions = guest_owned_regions(resources);
        append_arch_owned_regions(&mut owned_regions);
        map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
        map_arch_address_space(resources)?;
        vcpus.setup(resources, build_vcpu_setup_config)?;

        Ok(PreparedVm::new(vcpus, devices))
    })
}

fn build_vcpu_setup_config(
    config: &AxVMConfig,
    memory_regions: &[crate::vm::VMMemoryRegion],
) -> AxVmResult<<super::AxvmX86Vcpu as VmArchVcpuOps>::SetupConfig> {
    let mut setup_config = X86VCpuSetupConfig {
        emulate_com1: config
            .emu_devices()
            .iter()
            .any(|device| device.emu_type == EmulatedDeviceType::Console),
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
    for port in config.pass_through_ports() {
        x86_result(setup_config.add_passthrough_port_range(port.base, port.length))
            .map_err(|error| AxVmError::vcpu("configure passthrough port range", error))?;
    }
    Ok(setup_config)
}

fn register_interrupt_controller(
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
    interrupt_topology: &axdevice::InterruptTopology,
) -> AxVmResult {
    let mut ioapic_configs = config
        .emu_devices()
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::X86IoApic);
    let Some(ioapic_config) = ioapic_configs.next() else {
        return Ok(());
    };
    if ioapic_configs.next().is_some() {
        return Err(AxVmError::invalid_config(
            "an x86 VM may register only one IOAPIC controller",
        ));
    }

    let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
        x86_vlapic::X86GuestPhysAddr::from_usize(ioapic_config.base_gpa),
        Some(ioapic_config.length),
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
        ioapic_config.base_gpa, ioapic_config.length
    );
    Ok(())
}

fn register_arch_devices(
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
    interrupt_topology: &axdevice::InterruptTopology,
) -> AxVmResult {
    for port in config.pass_through_ports() {
        let passthrough = Arc::new(super::port::HostPortPassthrough::new(
            port.base,
            port.length,
        )?);
        let range = passthrough.address_range();
        debug!(
            "PT port region: [{:#x}~{:#x}]",
            range.start.number(),
            range.end.number(),
        );
        devices
            .register(PortDeviceAdapter::from_arc(passthrough))
            .map_err(|err| {
                ax_err_type!(InvalidInput, alloc::format!("register PT port: {err:?}"))
            })?;
    }
    for device_config in config.emu_devices() {
        super::register_arch_device(device_config, devices, interrupt_topology)?;
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
