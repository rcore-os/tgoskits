//! RISC-V implementations of AxVM platform capability hooks.

use alloc::{format, vec::Vec};

use super::Riscv64Arch;
use crate::{
    AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform},
    ax_err_type,
};

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

pub(super) fn decode_plic_source(specifier: &[u32]) -> Option<u32> {
    specifier.first().copied().filter(|source| *source != 0)
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    let host_fdt = super::fdt::core::try_get_host_fdt()
        .map(fdt_edit::Fdt::from_bytes)
        .transpose()
        .map_err(|err| {
            ax_err_type!(
                InvalidData,
                format!("Failed to parse host FDT while updating guest FDT: {err:#?}")
            )
        })?;
    let guest_fdt = super::fdt::core::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        crate_config,
        None,
        false,
    )?;
    super::fdt::ensure_chosen_from_host(guest_fdt, host_fdt.as_ref())
}

pub(super) fn patch_provided_fdt(
    provided_dtb: &[u8],
    _host_dtb: Option<&[u8]>,
    _crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    Ok(provided_dtb.to_vec())
}

#[cfg(test)]
mod tests {
    #[test]
    fn plic_interrupt_uses_first_nonzero_fdt_cell() {
        assert_eq!(super::decode_plic_source(&[8]), Some(8));
        assert_eq!(super::decode_plic_source(&[0]), None);
        assert_eq!(super::decode_plic_source(&[]), None);
    }
}
