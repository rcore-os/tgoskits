//! x86_64 VM resource creation and initialization.

use ax_memory_addr::PAGE_SIZE_4K;
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, MappingFlags, NestedPagingConfig, VmArchVcpuOps,
};
use x86_vcpu::{
    X86_LOCAL_APIC_GPA, X86_LOCAL_APIC_SIZE, X86GuestMemoryRegion, X86GuestPhysAddr,
    X86HostVirtAddr, X86VcpuCreateConfig, X86VcpuSetupConfig,
};

use super::{
    X86_64Arch, nested_paging, x86_apic_access_page_addr, x86_apic_access_page_gpa,
    x86_requires_apic_access_page, x86_result,
};
use crate::{
    AxVmError, AxVmResult, ax_err,
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
        let levels = guest_page_table_levels(&placements)?;
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
                let (factories, interrupt_controller) = prepare_device_bootstrap(vm)?;
                init_vm_with(vm, &factories, interrupt_controller)
            }
            VmInitRequest::Provided { factories } => {
                let configs = vm.with_config(|config| config.emu_devices().clone());
                let interrupt_controller =
                    super::register_device_factories(vm.id(), &configs, factories)?;
                init_vm_with(vm, factories, interrupt_controller)
            }
        }
    }
}

fn prepare_device_bootstrap(
    vm: &AxVM,
) -> AxVmResult<(
    axdevice::DeviceFactoryRegistry,
    alloc::sync::Arc<dyn axdevice_base::VirtualInterruptController>,
)> {
    let mut factories = default_device_factories(vm)?;
    let configs = vm.with_config(|config| config.emu_devices().clone());
    let interrupt_controller = super::register_device_factories(vm.id(), &configs, &mut factories)?;
    Ok((factories, interrupt_controller))
}

fn init_vm_with(
    vm: &AxVM,
    factories: &axdevice::DeviceFactoryRegistry,
    interrupt_controller: alloc::sync::Arc<dyn axdevice_base::VirtualInterruptController>,
) -> AxVmResult {
    complete_vm_init(
        vm,
        interrupt_controller,
        |resources, interrupt_controller| {
            let placements = vcpu_placements(resources);
            let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VcpuCreateConfig))?;
            let extra_devices = arch_extra_device_configs(resources.config());
            let devices = PreparedDevices::build_common_with_extra(
                resources,
                factories,
                interrupt_controller,
                &extra_devices,
                vm.device_access_ports(),
            )?;
            validate_guest_dtb(resources)?;

            let mut owned_regions = guest_owned_regions(resources);
            append_arch_owned_regions(&mut owned_regions);
            map_guest_address_space(vm, resources, devices.devices(), &owned_regions)?;
            map_arch_address_space(resources)?;
            vcpus.setup(resources, build_vcpu_setup_config)?;

            Ok(PreparedVm::new(vcpus, devices))
        },
    )
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

fn arch_extra_device_configs(config: &AxVMConfig) -> alloc::vec::Vec<EmulatedDeviceConfig> {
    config
        .pass_through_ports()
        .iter()
        .map(|port| {
            debug!(
                "PT port region: [{:#x}~{:#x}]",
                port.base,
                port.base as u32 + port.length as u32 - 1,
            );
            EmulatedDeviceConfig {
                name: alloc::format!("x86-port-passthrough-{:#x}", port.base),
                base_gpa: port.base as usize,
                length: port.length as usize,
                irq_id: 0,
                emu_type: EmulatedDeviceType::X86PortPassthrough,
                cfg_list: alloc::vec![],
            }
        })
        .collect()
}

fn append_arch_owned_regions(regions: &mut alloc::vec::Vec<GuestOwnedRegion>) {
    regions.push(GuestOwnedRegion::new(
        X86_LOCAL_APIC_GPA,
        X86_LOCAL_APIC_SIZE,
        crate::layout::VmRegionKind::Reserved,
    ));
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

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    crate::architecture::minimum_recorded_target_cpu_capability(
        "x86 nested page-table levels",
        vcpu_mappings,
        |cpu_id| {
            crate::percpu::select_cpu_virtualization_capability(cpu_id, |levels, _, _| {
                levels as u64
            })
        },
    )
    .map(|levels| levels as usize)
    .map_err(|error| {
        crate::architecture::unsupported_target_cpu_capability(
            "select x86 target CPU capability",
            error,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svm_reserves_the_local_apic_trap_region() {
        let mut regions = alloc::vec::Vec::new();

        append_arch_owned_regions(&mut regions);

        assert_eq!(
            regions,
            [GuestOwnedRegion::new(
                X86_LOCAL_APIC_GPA,
                X86_LOCAL_APIC_SIZE,
                crate::layout::VmRegionKind::Reserved,
            )]
        );

        let layout = crate::layout::build_address_layout(
            axvm_types::AddressSpacePolicy::Passthrough,
            0,
            0x1_0000_0000,
            &[],
            &[],
            &regions,
            &[],
        )
        .unwrap();
        assert_eq!(
            layout
                .mappings()
                .iter()
                .map(|mapping| (mapping.gpa.as_usize(), mapping.size))
                .collect::<alloc::vec::Vec<_>>(),
            [
                (0, X86_LOCAL_APIC_GPA),
                (
                    X86_LOCAL_APIC_GPA + X86_LOCAL_APIC_SIZE,
                    0x1_0000_0000 - X86_LOCAL_APIC_GPA - X86_LOCAL_APIC_SIZE,
                ),
            ]
        );
    }
}
