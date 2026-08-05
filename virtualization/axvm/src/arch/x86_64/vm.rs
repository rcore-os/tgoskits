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
            PreparedVm,
            address_space::{guest_owned_regions, map_guest_address_space},
            complete_vm_init,
            device_plan::{SimpleVmPlan, VmDevicePlan, machine_factory_registry},
            devices::PreparedDevices,
            validate_guest_dtb,
            vcpus::{PreparedVcpus, vcpu_placements},
        },
    },
};

pub(crate) type X86VmPlan = SimpleVmPlan;

impl X86_64Arch {
    pub(crate) fn create_vm_resources(
        config: AxVMConfig,
        fw_cfg_payload: alloc::sync::Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        let device_plan = plan_devices(&config, fw_cfg_payload)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = nested_paging::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, device_plan, |root_paddr| {
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
        init_vm_with(vm)
    }
}

fn plan_devices(
    config: &AxVMConfig,
    fw_cfg_payload: alloc::sync::Arc<axdevice::FwCfgPayloadSlot>,
) -> AxVmResult<X86VmPlan> {
    let extra = arch_extra_device_configs(config);
    let configs = x86_device_order(config, &extra)?;
    let low_memory_size = super::cmos::guest_low_memory_size(config)?;
    let mut factories = machine_factory_registry(config)?;
    factories.register(alloc::sync::Arc::new(
        axdevice::FwCfgPayloadFactory::deferred_pio(fw_cfg_payload),
    ))?;
    super::register_device_factories(config.id(), &configs, &mut factories)?;
    super::acpi_pm_timer::register_factory(&configs, &mut factories)?;
    super::cmos::register_factory(&configs, low_memory_size, &mut factories)?;
    super::pci_config::register_factory(&configs, &mut factories)?;
    super::pic::register_factory(&configs, &mut factories)?;
    Ok(SimpleVmPlan::new(VmDevicePlan::with_pools_for_vm(
        config,
        &configs,
        factories,
        Some(EmulatedDeviceType::X86IoApic),
        &[],
        super::resource_pools::create(config)?,
    )?))
}

fn init_vm_with(vm: &AxVM) -> AxVmResult {
    complete_vm_init(vm, |resources| {
        let placements = vcpu_placements(resources);
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VcpuCreateConfig))?;
        let devices = PreparedDevices::build_planned(resources, vm.device_access_ports())?;
        let interrupt_controller = devices
            .devices()
            .interrupt_controller(axdevice_base::InterruptControllerId::new(0))?;
        validate_guest_dtb(resources)?;

        let mut owned_regions = guest_owned_regions(resources);
        append_arch_owned_regions(&mut owned_regions);
        map_guest_address_space(vm, resources, &owned_regions)?;
        map_arch_address_space(resources)?;
        let intercepted_ports = resolved_port_intercepts(resources)?;
        vcpus.setup(resources, |config, memory_regions| {
            build_vcpu_setup_config(config, memory_regions, &intercepted_ports)
        })?;

        Ok(PreparedVm::new(vcpus, devices, interrupt_controller))
    })
}

fn x86_device_order(
    config: &AxVMConfig,
    extra: &[EmulatedDeviceConfig],
) -> AxVmResult<alloc::vec::Vec<EmulatedDeviceConfig>> {
    let mut ordered = alloc::vec::Vec::new();
    let mut controllers = config
        .emu_devices()
        .iter()
        .filter(|device| device.emu_type == EmulatedDeviceType::X86IoApic);
    ordered.push(
        controllers
            .next()
            .cloned()
            .ok_or_else(|| AxVmError::invalid_config("x86 machine profile has no IOAPIC"))?,
    );
    if controllers.next().is_some() {
        return Err(AxVmError::invalid_config(
            "x86 machine profile has more than one IOAPIC",
        ));
    }
    ordered.extend(
        config
            .emu_devices()
            .iter()
            .filter(|device| device.emu_type != EmulatedDeviceType::X86IoApic)
            .cloned(),
    );
    ordered.extend_from_slice(extra);
    Ok(ordered)
}

fn build_vcpu_setup_config(
    _config: &AxVMConfig,
    memory_regions: &[crate::vm::VMMemoryRegion],
    intercepted_ports: &[(u16, u16)],
) -> AxVmResult<<super::AxvmX86Vcpu as VmArchVcpuOps>::SetupConfig> {
    let mut setup_config = X86VcpuSetupConfig {
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
    for &(base, size) in intercepted_ports {
        x86_result(setup_config.add_intercepted_port_range(base, size))
            .map_err(|error| AxVmError::vcpu("configure resolved device port intercept", error))?;
    }
    Ok(setup_config)
}

fn resolved_port_intercepts(resources: &AxVMResources) -> AxVmResult<alloc::vec::Vec<(u16, u16)>> {
    let graph = resources.planned_devices().graph();
    let mut ranges = alloc::vec::Vec::new();
    for node in graph.nodes() {
        ranges.extend(
            graph
                .resources_for(node.id())?
                .pio_ranges()
                .map(|(_, base, size)| (base, size)),
        );
    }
    Ok(ranges)
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
                name: std::format!("x86-port-passthrough-{:#x}", port.base),
                base_gpa: port.base as usize,
                length: port.length as usize,
                irq_id: 0,
                emu_type: EmulatedDeviceType::X86PortPassthrough,
                cfg_list: std::vec![],
            }
        })
        .collect()
}

fn append_arch_owned_regions(regions: &mut std::vec::Vec<GuestOwnedRegion>) {
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
        let mut regions = std::vec::Vec::new();

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
                .collect::<std::vec::Vec<_>>(),
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
