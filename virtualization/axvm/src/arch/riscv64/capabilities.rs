//! RISC-V implementations of AxVM platform capability hooks.

use alloc::vec::Vec;

use super::Riscv64Arch;
use crate::{
    AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform, HostTimePlatform},
    ax_err_type,
};

impl HostTimePlatform for Riscv64Arch {}

impl BootImagePlatform for Riscv64Arch {
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

impl GuestBootPlatform for Riscv64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::AxVMCrateConfig,
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

pub(crate) fn host_fdt_bytes() -> Option<&'static [u8]> {
    ax_std::os::arceos::modules::ax_hal::dtb::get_fdt().map(|fdt| fdt.as_slice())
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    super::fdt::core::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        crate_config,
        None,
        true,
    )
}
