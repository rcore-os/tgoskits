//! AArch64 implementations of AxVM platform capability hooks.

use alloc::format;

use super::Aarch64Arch;
use crate::{
    AxVmError, AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform, HostTimePlatform, PhysicalSpiPlatform},
    ax_err_type,
};

impl HostTimePlatform for Aarch64Arch {}

impl PhysicalSpiPlatform for Aarch64Arch {
    fn physical_spi_target_mpidr(vm: &crate::vm::AxVM) -> AxVmResult<Option<usize>> {
        let placement = crate::manager::vcpu_task_placement(vm.id(), 0).ok_or_else(|| {
            AxVmError::resource_unavailable(
                "guest CPU partition",
                format_args!("VM[{}] vCPU[0] has no validated task affinity", vm.id()),
            )
        })?;
        let enabled_cpu_mask = crate::percpu::enabled_cpu_mask();
        let host_cpu = placement
            .affinity
            .single_enabled_cpu(enabled_cpu_mask)
            .ok_or_else(|| {
                AxVmError::interrupt(
                    "route passthrough device IRQ",
                    format_args!(
                        "VM[{}] vCPU[0] affinity {:?} must select exactly one CPU from enabled \
                         mask {enabled_cpu_mask:#x}",
                        vm.id(),
                        placement.affinity
                    ),
                )
            })?;
        let target_mpidr = someboot::smp::cpu_idx_to_id(host_cpu).ok_or_else(|| {
            AxVmError::resource_unavailable(
                "host CPU topology",
                format_args!("logical CPU {host_cpu} has no hardware MPIDR"),
            )
        })?;
        Ok(Some(target_mpidr))
    }

    fn with_physical_spi_controller<T, F>(operation: F) -> AxVmResult<Option<T>>
    where
        F: FnOnce(&mut dyn crate::vm::PassthroughSpiController) -> AxVmResult<T>,
    {
        super::gic::with_passthrough_spi_controller(operation).map(Some)
    }
}

impl BootImagePlatform for Aarch64Arch {
    fn load_guest_dtb(
        loader: &crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        let bytes = dtb.as_bytes();
        let source = core::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        super::fdt::core::update_fdt(source, bytes.len(), loader.vm.clone(), &loader.config)
    }
}

impl GuestBootPlatform for Aarch64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::AxVMCrateConfig,
        provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        super::fdt::handle_fdt_operations(vm_config, vm_create_config, provider)
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}

pub(super) fn decode_gic_spi(specifier: &[u32]) -> Option<u32> {
    (specifier.first().copied() == Some(0))
        .then(|| specifier.get(1).copied())
        .flatten()
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<alloc::vec::Vec<u8>> {
    let initrd = vm.with_config(|config| {
        super::fdt::initrd_start_size_from_image_config(config.image_config.ramdisk.as_ref())
    });
    super::fdt::core::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        crate_config,
        initrd,
        true,
    )
}

pub(super) fn patch_provided_fdt(
    provided_dtb: &[u8],
    host_dtb: Option<&[u8]>,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<alloc::vec::Vec<u8>> {
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
