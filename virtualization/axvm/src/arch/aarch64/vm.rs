//! AArch64 VM resource creation and initialization.

use alloc::sync::Arc;

use arm_vcpu::{ArmVcpuCreateConfig, ArmVcpuSetupConfig};
use axdevice_base::DeviceRegistry as _;
use axvm_types::{NestedPagingConfig, VMInterruptMode, VmArchVcpuOps};

use super::{Aarch64Arch, npt};
use crate::{
    AxVMRef, AxVmError, AxVmResult, VmStatus, ax_err,
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
    _vm: &AxVM,
    config: &AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
) -> AxVmResult {
    if config.interrupt_mode() != VMInterruptMode::Passthrough {
        register_virtual_timers(devices)?;
    }
    Ok(())
}

pub(crate) fn activate_guest_irq_routes(vm: &AxVMRef) -> AxVmResult {
    if !matches!(vm.status(), VmStatus::Ready | VmStatus::Stopped) {
        return ax_err!(
            BadState,
            "AArch64 guest IRQ routes can only activate before or between VM runtime generations"
        );
    }
    if vm.with_config(|config| config.interrupt_mode() != VMInterruptMode::Passthrough) {
        return Ok(());
    }
    if vm.with_config(|config| config.pass_through_spis().is_empty()) {
        return Ok(());
    }

    let devices = vm.get_devices()?;
    vm.with_config(|config| assign_passthrough_spis(config, &devices))?;
    Ok(())
}

struct PassthroughSpiTarget {
    logical_cpu: usize,
    affinity: (u8, u8, u8, u8),
}

fn passthrough_spi_target(config: &AxVMConfig) -> AxVmResult<PassthroughSpiTarget> {
    let (_, host_cpu_mask, host_mpidr) = config
        .phys_cpu_ls
        .get_vcpu_affinities_pcpu_ids()
        .into_iter()
        .find(|(vcpu_id, ..)| *vcpu_id == 0)
        .ok_or_else(|| {
            AxVmError::invalid_config("AArch64 passthrough SPI routing requires vCPU0")
        })?;
    let host_cpu_mask = host_cpu_mask.ok_or_else(|| {
        AxVmError::invalid_config(
            "AArch64 passthrough SPI routing requires a fixed host CPU for vCPU0",
        )
    })?;
    if !host_cpu_mask.is_power_of_two() {
        return Err(AxVmError::invalid_config(format_args!(
            "AArch64 passthrough SPI routing requires one host CPU, got mask {host_cpu_mask:#x}"
        )));
    }

    let mpidr = host_mpidr as u64;
    Ok(PassthroughSpiTarget {
        logical_cpu: host_cpu_mask.trailing_zeros() as usize,
        affinity: (
            ((mpidr >> 32) & 0xff) as u8,
            ((mpidr >> 16) & 0xff) as u8,
            ((mpidr >> 8) & 0xff) as u8,
            (mpidr & 0xff) as u8,
        ),
    })
}

fn assign_passthrough_spis(config: &AxVMConfig, devices: &axdevice::AxVmDevices) -> AxVmResult {
    let target = passthrough_spi_target(config)?;
    let Some(gicd) = devices
        .devices()
        .find_map(|device| device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>())
    else {
        if config.pass_through_spis().is_empty() {
            return Ok(());
        }
        return ax_err!(
            BadState,
            "cannot assign passthrough SPIs without a VGIC distributor"
        );
    };

    for spi in config.pass_through_spis() {
        let irq = spi.checked_add(32).ok_or_else(|| {
            AxVmError::invalid_input("assign passthrough SPI", "SPI number overflow")
        })?;
        gicd.assign_irq(irq, target.logical_cpu, target.affinity)
            .map_err(|error| AxVmError::interrupt("assign passthrough SPI", error))?;
    }
    Ok(())
}

pub(crate) fn revoke_guest_irq_routes(vm: &AxVMRef) -> AxVmResult {
    const MAX_GICD_RWP_POLLS: usize = 10_000;

    if !matches!(vm.status(), VmStatus::Ready | VmStatus::Stopped) {
        return ax_err!(
            BadState,
            "AArch64 guest IRQ routes can only be revoked after all vCPUs stop"
        );
    }

    let has_passthrough_spis = vm.with_config(|config| !config.pass_through_spis().is_empty());
    let devices = vm.get_devices()?;
    let Some(gicd) = devices
        .devices()
        .find_map(|device| device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>())
    else {
        if has_passthrough_spis {
            return ax_err!(
                BadState,
                "cannot revoke passthrough SPIs without a VGIC distributor"
            );
        }
        return Ok(());
    };

    let mut revocation = gicd
        .begin_assigned_spi_revocation()
        .map_err(|error| AxVmError::interrupt("begin AArch64 SPI revocation", error))?;
    for _ in 0..MAX_GICD_RWP_POLLS {
        match revocation
            .poll()
            .map_err(|error| AxVmError::interrupt("poll AArch64 SPI revocation", error))?
        {
            arm_vgic::v3::vgicd::SpiRevocationPoll::Pending(next) => {
                revocation = next;
                crate::host::task::yield_now();
            }
            arm_vgic::v3::vgicd::SpiRevocationPoll::Complete(proof) => {
                debug!(
                    "VM[{}] released {} AArch64 passthrough SPIs",
                    vm.id(),
                    proof.released_spi_count()
                );
                return Ok(());
            }
        }
    }

    Err(AxVmError::interrupt(
        "poll AArch64 SPI revocation",
        "GICD_CTLR.RWP did not clear before the bounded retry limit",
    ))
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
