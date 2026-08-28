//! x86_64 VM resource creation and initialization.

use ax_memory_addr::PAGE_SIZE_4K;
use axdevice::{DeviceFirmwareBinding, DeviceNodeId, DeviceNodeSpec};

use super::*;
use crate::{
    config::*,
    layout::*,
    vm::{
        prepare::{device_plan::*, devices::*, vcpus::*, *},
        *,
    },
};

pub(crate) type X86VmPlan = SimpleVmPlan;

const ARCH_OWNED_REGIONS: [GuestOwnedRegion; 1] = [GuestOwnedRegion::new(
    X86_LOCAL_APIC_GPA,
    X86_LOCAL_APIC_SIZE,
    crate::layout::VmRegionKind::Reserved,
)];

impl X86_64Arch {
    pub(crate) fn create_vm_resources(
        config: &mut AxVMConfig,
        fw_cfg_payload: std::sync::Arc<axdevice::FwCfgPayloadSlot>,
    ) -> AxVmResult<AxVMResources> {
        #[cfg(feature = "host-fs")]
        apply_host_serial(config)?;
        let device_plan = plan_devices(config, fw_cfg_payload)?;
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = nested_paging::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config.id(), page_table, device_plan, |root_paddr| {
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
        vm.prepare_resources_with(|resources, config| {
            let placements = resources.vcpu_placements(config);
            let vcpus = PreparedVcpus::create(vm.id(), &placements, |_| Ok(X86VcpuCreateConfig))?;
            let devices = PreparedDevices::build_planned(resources, vm.device_access_ports())?;
            let interrupt_controller = devices
                .devices()
                .interrupt_controller(axdevice_base::InterruptControllerId::new(0))?;
            resources.prepare_guest_address_space(vm.id(), config, &ARCH_OWNED_REGIONS)?;
            resources.map_arch_address_space()?;
            let intercepted_ports = resources.resolved_port_intercepts()?;
            let intercepted_mmio = resources.resolved_mmio_intercepts()?;
            vcpus.setup(resources, config, |config, memory_regions| {
                build_vcpu_setup_config(
                    config,
                    memory_regions,
                    &intercepted_ports,
                    &intercepted_mmio,
                )
            })?;

            Ok(PreparedVm::new(vcpus, devices, interrupt_controller))
        })
    }
}

#[cfg(feature = "host-fs")]
fn apply_host_serial(config: &mut AxVMConfig) -> AxVmResult {
    let Some(serial) = ax_driver::probe::acpi::with_acpi(|acpi| acpi.serial_console()) else {
        return Ok(());
    };
    let Some(serial) = serial.map_err(|error| {
        AxVmError::invalid_config(std::format!(
            "failed to parse host ACPI serial console: {error}"
        ))
    })?
    else {
        return Ok(());
    };
    let snapshot = crate::machine::host_serial_from_acpi(serial, config.serial_profile())?;
    config.replace_machine_serial(snapshot.profile, Some(snapshot.identity))
}

fn plan_devices(
    config: &AxVMConfig,
    fw_cfg_payload: std::sync::Arc<axdevice::FwCfgPayloadSlot>,
) -> AxVmResult<X86VmPlan> {
    let low_memory_size = super::cmos::guest_low_memory_size(config)?;
    let controller_id = DeviceNodeId::new("ioapic")?;
    let mut nodes = std::vec![
        DeviceNodeSpec::virtual_device(
            controller_id.clone(),
            super::ioapic_model(config.id(), 0xfec0_0000, 0x1000),
        )
        .with_firmware_binding(DeviceFirmwareBinding::AcpiDevice("IOAPIC".into())),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("fw-cfg")?,
            std::sync::Arc::new(axdevice::FwCfgPayloadFactory::deferred_pio(
                GuestPhysAddr::from(0x510),
                0x0c,
                fw_cfg_payload,
            )),
        )
        .with_firmware_binding(DeviceFirmwareBinding::AcpiDevice("\\_SB.FWCF".into())),
        DeviceNodeSpec::virtual_device(DeviceNodeId::new("pit")?, super::pit_model(config.id()),),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("pic")?,
            std::sync::Arc::new(super::pic::X86PicModel),
        ),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("cmos")?,
            std::sync::Arc::new(super::cmos::X86CmosModel::new(low_memory_size)),
        ),
        DeviceNodeSpec::virtual_device(
            DeviceNodeId::new("acpi-pm-timer")?,
            std::sync::Arc::new(super::acpi_pm_timer::X86AcpiPmTimerModel),
        )
        .with_dependency(controller_id.clone()),
    ];
    for port in config.pass_through_ports() {
        let id = DeviceNodeId::new(std::format!("host-port-{:x}", port.base))?;
        nodes.push(DeviceNodeSpec::virtual_device(
            id,
            std::sync::Arc::new(super::port::HostPortPassthroughDeviceModel::new(
                port.base,
                port.length,
            )),
        ));
    }
    crate::configured::append_configured_devices(
        config,
        &mut nodes,
        &controller_id,
        axdevice_base::InterruptControllerId::new(0),
    )?;
    Ok(SimpleVmPlan::new(VmDevicePlan::with_pci_host_for_vm(
        config,
        nodes,
        &[],
        super::resource_pools::create(config)?,
        super::pci_config::provider()?,
    )?))
}

fn build_vcpu_setup_config(
    _config: &AxVMConfig,
    memory_regions: &[crate::vm::VMMemoryRegion],
    intercepted_ports: &[(u16, u16)],
    intercepted_mmio: &[(X86GuestPhysAddr, usize)],
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
    for &(base, size) in intercepted_mmio {
        x86_result(setup_config.add_intercepted_mmio_range(base, size))
            .map_err(|error| AxVmError::vcpu("configure resolved device MMIO intercept", error))?;
    }
    Ok(setup_config)
}

impl AxVMResources {
    fn resolved_port_intercepts(&self) -> AxVmResult<std::vec::Vec<(u16, u16)>> {
        let graph = self.planned_devices().graph();
        let mut ranges = std::vec::Vec::new();
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

    fn resolved_mmio_intercepts(&self) -> AxVmResult<std::vec::Vec<(X86GuestPhysAddr, usize)>> {
        let graph = self.planned_devices().graph();
        let mut ranges = std::vec::Vec::new();
        for node in graph.nodes() {
            for (_, base, size) in graph.resources_for(node.id())?.mmio_ranges() {
                let base = usize::try_from(base).map_err(|_| {
                    AxVmError::invalid_config("planned device MMIO GPA does not fit usize")
                })?;
                let size = usize::try_from(size).map_err(|_| {
                    AxVmError::invalid_config("planned device MMIO size does not fit usize")
                })?;
                ranges.push((X86GuestPhysAddr::from_usize(base), size));
            }
        }
        Ok(ranges)
    }

    fn map_arch_address_space(&mut self) -> AxVmResult {
        if x86_requires_apic_access_page()? {
            let gpa = x86_apic_access_page_gpa()?;
            self.address_space
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
        let layout = crate::layout::build_address_layout(
            axvm_types::AddressSpacePolicy::Passthrough,
            0,
            0x1_0000_0000,
            &[],
            &[],
            &ARCH_OWNED_REGIONS,
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

    #[test]
    fn build_vcpu_setup_config_forwards_resolved_device_mmio_ranges() {
        let config = AxVMConfig::default_for_test(1, "x86-mmio-setup-test");
        let bar0_base = X86GuestPhysAddr::from_usize(0x8000_0000);
        let bar0_size = 0x1000;

        let setup_config =
            build_vcpu_setup_config(&config, &[], &[], &[(bar0_base, bar0_size)]).unwrap();

        let ranges = setup_config
            .intercepted_mmio_ranges()
            .collect::<std::vec::Vec<_>>();
        assert_eq!(
            ranges,
            std::vec![X86InterceptedMmioRange {
                base: bar0_base,
                size: bar0_size,
            }]
        );
    }
}
