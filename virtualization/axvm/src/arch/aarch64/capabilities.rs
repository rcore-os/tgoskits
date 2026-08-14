//! AArch64 implementations of AxVM platform capability hooks.

use std::format;

use super::Aarch64Arch;
use crate::{architecture::*, *};

impl HostTimePlatform for Aarch64Arch {}

impl MachinePlatform for Aarch64Arch {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture =
        crate::machine::MachineArchitecture::Aarch64;
}

impl BootImagePlatform for Aarch64Arch {
    fn load_guest_dtb(
        loader: &crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        let bytes = dtb.as_bytes();
        let source = std::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        super::fdt::core::update_fdt(source, bytes.len(), loader.vm.clone(), &loader.config)
    }
}

impl GuestBootPlatform for Aarch64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::GuestConfig,
        provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        super::fdt::core::prepare_dtb_guest(vm_config, vm_create_config, provider)
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}

pub(super) fn resolve_cpu_index(hardware_cpu_id: usize) -> Option<usize> {
    ax_std::os::arceos::modules::ax_hal::topology::resolve_cpu_index(hardware_cpu_id)
}

pub(super) fn host_cpu_count() -> usize {
    ax_std::os::arceos::modules::ax_hal::cpu_num()
}

pub(super) fn decode_gic_spi(specifier: &[u32]) -> Option<super::fdt::core::DecodedInterrupt> {
    if specifier.first().copied() != Some(0) {
        return None;
    }
    let source = specifier.get(1).copied()?;
    let flags = specifier.get(2).copied().unwrap_or(4);
    let trigger = if flags & 0b11 != 0 {
        axdevice_base::InterruptTriggerMode::EdgeTriggered
    } else {
        axdevice_base::InterruptTriggerMode::LevelTriggered
    };
    Some(super::fdt::core::DecodedInterrupt { source, trigger })
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<std::vec::Vec<u8>> {
    let initrd = vm.with_config(|config| {
        super::fdt::initrd_start_size_from_image_config(config.image_config.ramdisk.as_ref())
    });
    let (
        serial_profile,
        serial_identity,
        additional_serials,
        ivc_channels,
        gic_profile,
        timer_profile,
    ) = vm.with_architecture_plan(|plan| {
        Ok((
            plan.serial_profile(),
            plan.serial_fdt_identity().cloned(),
            plan.serial_devices()
                .iter()
                .filter(|serial| serial.id() != "console0")
                .map(crate::machine::ResolvedSerialDevice::profile)
                .collect::<std::vec::Vec<_>>(),
            plan.ivc_channels().to_vec(),
            plan.gic_profile().clone(),
            plan.timer_profile().clone(),
        ))
    })?;
    super::fdt::core::create::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        &ivc_channels,
        crate_config,
        serial_profile,
        serial_identity.as_ref(),
        &additional_serials,
        Some(&gic_profile),
        None,
        Some(&timer_profile),
        initrd,
        true,
    )
}

pub(super) fn patch_provided_fdt(
    provided_dtb: &[u8],
    host_dtb: Option<&[u8]>,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<std::vec::Vec<u8>> {
    let provided_fdt = fdt_edit::Fdt::from_bytes(provided_dtb).map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse provided DTB image: {err:#?}")
        )
    })?;
    let host_fdt = host_dtb
        .map(fdt_edit::Fdt::from_bytes)
        .transpose()
        .map_err(|err| {
            ax_err_type!(
                InvalidData,
                format!("Failed to parse host DTB image: {err:#?}")
            )
        })?;
    super::fdt::update_cpu_node(&provided_fdt, host_fdt.as_ref(), crate_config)
}
