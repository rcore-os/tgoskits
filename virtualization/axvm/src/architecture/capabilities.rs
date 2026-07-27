//! Small capability boundaries implemented by the selected guest architecture.

use crate::AxVmResult;

/// Guest firmware preparation performed before common VM memory loading.
pub(crate) trait GuestBootPlatform {
    fn init_guest_boot_resources() {}

    fn prepare_guest_boot(
        _vm_config: &mut crate::config::AxVMConfig,
        _vm_create_config: &mut axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        Ok(None)
    }
}

/// Architecture-specific guest image planning layered over common byte loading.
pub(crate) trait BootImagePlatform {
    fn default_boot_firmware_load_gpa(
        _config: &axvmconfig::AxVMCrateConfig,
    ) -> Option<axvm_types::GuestPhysAddr> {
        None
    }

    fn load_images_from_memory(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
        images: crate::boot::StaticVmImage,
    ) -> AxVmResult {
        loader.load_standard_images_from_memory(images, Self::load_guest_dtb)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    ) -> AxVmResult {
        loader.load_standard_images_from_filesystem(Self::load_guest_dtb)
    }

    fn load_guest_dtb(
        _loader: &crate::boot::images::ImageLoaderCore<'_>,
        _dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        Ok(())
    }

    fn is_x86_linux_image_config(
        _config: &axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> bool {
        false
    }
}
