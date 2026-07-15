//! AArch64 implementations of AxVM platform capability hooks.

use alloc::format;

use ax_std::os::arceos::modules::ax_hal;
use axvm_types::VMInterruptMode;

use super::Aarch64Arch;
use crate::{
    AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform, HostTimePlatform},
    ax_err_type,
};

impl HostTimePlatform for Aarch64Arch {}

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
        let guest_dtb = super::fdt::core::prepare_dtb_guest(vm_config, vm_create_config, provider)?;
        if vm_config.interrupt_mode() == VMInterruptMode::Passthrough {
            let host_ipi = ax_hal::irq::ipi_irq().hwirq.0;
            let host_timer = ax_hal::time::irq_num().hwirq.0;
            let roles = super::gic::Aarch64InterruptRoles::discover(
                host_ipi,
                host_timer,
                super::fdt::try_get_host_fdt(),
                guest_dtb.as_ref().map(|dtb| dtb.as_bytes()),
                vm_config.host_reserved_intids(),
                vm_config.pass_through_spis(),
            )?;
            vm_config.arch_mut().set_interrupt_roles(roles);
        }
        Ok(guest_dtb)
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
    let patched = super::fdt::core::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        crate_config,
        initrd,
        true,
    )?;
    super::fdt::patch_emulated_timer_interrupts(&patched, crate_config.devices.interrupt_mode)
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
    let patched = super::fdt::update_cpu_node(&provided_fdt, host_fdt.as_ref(), crate_config)?;
    super::fdt::patch_emulated_timer_interrupts(&patched, crate_config.devices.interrupt_mode)
}
