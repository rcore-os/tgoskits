//! x86_64 VM resource creation and initialization.

use alloc::sync::Arc;

use ax_memory_addr::PAGE_SIZE_4K;
use axdevice_base::{BaseDeviceOps, DeviceRegistry as _, PortDeviceAdapter};
use axvm_types::{EmulatedDeviceType, MappingFlags, NestedPagingConfig, VmArchVcpuOps};
use x86_vcpu::{
    X86GuestMemoryRegion, X86GuestPhysAddr, X86HostVirtAddr, X86VcpuCreateConfig,
    X86VcpuSetupConfig,
};

use super::{
    X86_64Arch, nested_paging, x86_apic_access_page_addr, x86_apic_access_page_gpa,
    x86_requires_apic_access_page, x86_result,
};
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
        let page_table = nested_paging::NestedPageTable::new(levels)?;
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
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VcpuCreateConfig))?;
        let mut devices = PreparedDevices::build_common(resources, factories, interrupt_fabric)?;
        register_arch_devices(resources.config(), &mut devices.devices)?;
        devices.register_special_devices(vm)?;
        validate_guest_dtb(resources)?;

        let mut owned_regions = guest_owned_regions(resources);
        append_arch_owned_regions(&mut owned_regions)?;
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
    let mut setup_config = X86VcpuSetupConfig {
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

fn register_arch_devices(config: &AxVMConfig, devices: &mut axdevice::AxVmDevices) -> AxVmResult {
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
        super::register_arch_device(device_config, devices)?;
    }
    Ok(())
}

fn append_arch_owned_regions(regions: &mut alloc::vec::Vec<GuestOwnedRegion>) -> AxVmResult {
    if x86_requires_apic_access_page()? {
        let gpa = x86_apic_access_page_gpa()?;
        regions.push(GuestOwnedRegion::new(
            gpa.as_usize(),
            PAGE_SIZE_4K,
            crate::layout::VmRegionKind::Reserved,
        ));
    }
    Ok(())
}

fn map_arch_address_space(resources: &mut AxVMResources) -> AxVmResult {
    if x86_requires_apic_access_page()? {
        let gpa = x86_apic_access_page_gpa()?;
        resources
            .address_space
            .map_linear(
                gpa,
                x86_apic_access_page_addr()?,
                PAGE_SIZE_4K,
                MappingFlags::DEVICE | MappingFlags::READ | MappingFlags::WRITE,
            )
            .map_err(|error| AxVmError::memory("map x86 APIC access page", error))?;
    }
    Ok(())
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> usize {
    let mut levels = 4;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        levels = levels.min(crate::percpu::cpu_max_guest_page_table_levels(cpu_id).unwrap_or(4));
    }
    levels
}
