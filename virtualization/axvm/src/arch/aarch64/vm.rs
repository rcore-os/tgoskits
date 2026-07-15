//! AArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use arm_vcpu::{ArmVcpuCreateConfig, ArmVcpuSetupConfig};
use ax_cpumask::CpuMask;
use axdevice_base::DeviceRegistry as _;
use axvm_types::{NestedPagingConfig, VMInterruptMode, VmArchVcpuOps};

use super::{Aarch64Arch, npt};
use crate::{
    AxVmError, AxVmResult, ax_err,
    config::{AxVMConfig, hybrid_guest_intids},
    vm::{
        AxVM, AxVMResources, ConsoleIoPolicy, TEMP_MAX_VCPU_NUM,
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

impl Aarch64Arch {
    pub(crate) fn create_vm_resources(config: AxVMConfig) -> AxVmResult<AxVMResources> {
        let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let levels = guest_page_table_levels(&placements)?;
        let page_table = npt::NestedPageTable::new(levels)?;
        AxVMResources::from_page_table(config, page_table, |root_paddr| {
            nested_paging_config(root_paddr, levels, &placements)
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

impl AxVM {
    /// Returns whether this VM has a connectable emulated AArch64 console.
    pub fn has_connect_console(&self) -> bool {
        self.get_devices()
            .map(|devices| devices.has_aarch64_console())
            .unwrap_or(false)
    }

    /// Pushes host input into this running VM's emulated AArch64 console.
    pub fn push_console_input(&self, bytes: &[u8]) -> AxVmResult {
        ConsoleIoPolicy::ensure_running(self.status())?;
        let devices = self.get_devices()?;
        let pending_irq =
            ConsoleIoPolicy::require_console(devices.aarch64_console_push_input(bytes))?;
        let fabric_has_backend =
            self.with_resources(|resources| Ok(resources.interrupt_fabric()?.has_backend()))?;
        ConsoleIoPolicy::deliver_pending_irq(
            pending_irq,
            fabric_has_backend,
            |irq| self.pulse_interrupt(irq),
            |irq| {
                let mut targets = CpuMask::<TEMP_MAX_VCPU_NUM>::new();
                targets.set(0, true);
                self.inject_interrupt_to_vcpu(targets, irq)
            },
        )
    }

    /// Drains guest output from this running VM's emulated AArch64 console.
    pub fn drain_console_output(&self, output: &mut [u8]) -> AxVmResult<usize> {
        ConsoleIoPolicy::ensure_running(self.status())?;
        let devices = self.get_devices()?;
        ConsoleIoPolicy::require_console(devices.aarch64_console_drain_output(output))
    }
}

fn init_vm_with(
    vm: &AxVM,
    factories: &axdevice::DeviceFactoryRegistry,
    interrupt_fabric: crate::InterruptFabric,
) -> AxVmResult {
    complete_vm_init(vm, interrupt_fabric, |resources, interrupt_fabric| {
        let placements = vcpu_placements(resources);
        let dtb_addr = resources
            .config()
            .image_config()
            .dtb_load_gpa
            .unwrap_or_default();
        let vcpus = PreparedVcpus::create(vm.id(), &placements, |placement| {
            Ok(ArmVcpuCreateConfig {
                mpidr_el1: placement.phys_cpu_id as _,
                dtb_addr: dtb_addr.as_usize(),
            })
        })?;
        let mut devices = PreparedDevices::build_common(resources, factories, interrupt_fabric)?;
        register_arch_devices(vm, resources.config(), &mut devices.devices)?;
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
) -> AxVmResult<<super::AxvmArmVcpu as VmArchVcpuOps>::SetupConfig> {
    let passthrough = config.interrupt_mode() == VMInterruptMode::Passthrough;
    Ok(ArmVcpuSetupConfig {
        passthrough_interrupt: passthrough,
        passthrough_timer: passthrough,
    })
}

fn register_arch_devices(
    vm: &AxVM,
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
) -> AxVmResult {
    let interrupt_mode = config.interrupt_mode();
    if interrupt_mode == VMInterruptMode::Passthrough {
        assign_passthrough_spis(vm, config, devices)?;
    } else {
        if interrupt_mode == VMInterruptMode::Hybrid {
            allow_hybrid_guest_spis(config, devices)?;
        }
        register_virtual_timers(devices)?;
    }
    Ok(())
}

fn allow_hybrid_guest_spis(config: &AxVMConfig, devices: &axdevice::AxVmDevices) -> AxVmResult {
    let Some(gicd) = devices
        .devices()
        .find_map(|device| device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>())
    else {
        return Ok(());
    };

    let routes = config.aarch64_hybrid_forwarded_irqs();
    let console_intid = devices.aarch64_console_irq().map(|irq| irq as u32);
    if console_intid.is_some_and(|intid| !(32..1020).contains(&intid)) {
        return ax_err!(InvalidInput, "AArch64 console IRQ is not a GIC SPI");
    }
    for intid in hybrid_guest_intids(routes, console_intid) {
        gicd.allow_guest_irq(intid)
            .map_err(|error| AxVmError::interrupt("allow Hybrid guest SPI", error))?;
    }
    Ok(())
}

fn assign_passthrough_spis(
    vm: &AxVM,
    config: &AxVMConfig,
    devices: &axdevice::AxVmDevices,
) -> AxVmResult {
    let cpu_id = vm.id() - 1; // FIXME: get the real CPU id.
    let Some(gicd) = devices
        .devices()
        .find_map(|device| device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>())
    else {
        warn!("Failed to assign SPIs: No VGicD found in device list");
        return Ok(());
    };

    for spi in config.pass_through_spis() {
        gicd.assign_irq(*spi + 32, cpu_id, (0, 0, 0, cpu_id as _))
            .map_err(|error| AxVmError::interrupt("assign passthrough SPI", error))?;
    }
    Ok(())
}

fn register_virtual_timers(devices: &mut axdevice::AxVmDevices) -> AxVmResult {
    for device in axdevice::create_vtimer_devices() {
        devices.register(Arc::from(device) as Arc<dyn axdevice_base::Device>)?;
    }
    Ok(())
}

fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxVmResult<usize> {
    let mut selected = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let levels = crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
            .unwrap_or_else(arm_vcpu::max_guest_page_table_levels);
        if levels == 0 {
            return ax_err!(
                Unsupported,
                "AArch64 nested paging is not enabled on target CPU"
            );
        }
        selected = selected.min(levels);
    }
    if selected == usize::MAX {
        selected = arm_vcpu::max_guest_page_table_levels();
    }
    match selected {
        3 | 4 => Ok(selected),
        _ => ax_err!(Unsupported, "unsupported AArch64 stage-2 page-table levels"),
    }
}

fn nested_paging_config(
    root_paddr: ax_memory_addr::PhysAddr,
    levels: usize,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
) -> AxVmResult<NestedPagingConfig> {
    let mut pa_bits = usize::MAX;
    for cpu_id in crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings) {
        let bits =
            crate::percpu::cpu_guest_phys_addr_bits(cpu_id).unwrap_or_else(arm_vcpu::pa_bits);
        pa_bits = pa_bits.min(bits);
    }
    if pa_bits == usize::MAX {
        pa_bits = arm_vcpu::pa_bits();
    }

    let gpa_bits = match levels {
        3 => 39,
        4 => 48,
        _ => return ax_err!(InvalidInput, "unsupported AArch64 stage-2 levels"),
    };
    Ok(NestedPagingConfig::new(
        root_paddr, levels, gpa_bits, pa_bits,
    ))
}
