//! Small capability boundaries implemented by the selected guest architecture.

use ax_errno::AxResult;

use super::ArchOps;

/// Guest firmware preparation performed before common VM memory loading.
pub(crate) trait GuestBootPlatform {
    fn init_guest_boot_resources() {}

    fn prepare_guest_boot(
        _vm_config: &mut crate::config::AxVMConfig,
        _vm_create_config: &mut axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> AxResult<Option<crate::boot::fdt::GuestDtbImage>> {
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
    ) -> AxResult {
        loader.load_standard_images_from_memory(images, Self::load_guest_dtb)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    ) -> AxResult {
        loader.load_standard_images_from_filesystem(Self::load_guest_dtb)
    }

    fn load_guest_dtb(
        _loader: &crate::boot::images::ImageLoaderCore<'_>,
        _dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxResult {
        Ok(())
    }

    fn is_x86_linux_image_config(
        _config: &axvmconfig::AxVMCrateConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> bool {
        false
    }
}

/// Architecture-owned interrupt fabric and device registration hooks.
pub(crate) trait DevicePlatform {
    fn configure_interrupt_fabric(
        _factories: &mut axdevice::DeviceFactoryRegistry,
        mode: axvm_types::VMInterruptMode,
        _configs: &[axvm_types::EmulatedDeviceConfig],
    ) -> AxResult<crate::InterruptFabric> {
        Ok(crate::InterruptFabric::new(mode))
    }

    fn register_devices(
        _vm: &crate::AxVM,
        _config: &crate::config::AxVMConfig,
        _devices: &mut axdevice::AxVmDevices,
    ) -> AxResult {
        Ok(())
    }
}

/// Architecture-owned guest address-space additions.
pub(crate) trait AddressSpacePlatform: ArchOps {
    fn append_owned_regions(_regions: &mut alloc::vec::Vec<crate::layout::GuestOwnedRegion>) {}

    fn map_address_space(
        _address_space: &mut axaddrspace::AddrSpace<Self::NestedPageTable>,
    ) -> AxResult {
        Ok(())
    }
}

/// Architecture-specific host timer policy used by the ArceOS adapter.
pub(crate) trait HostTimePlatform {
    fn set_oneshot_timer(deadline_ns: u64) {
        ax_std::os::arceos::modules::ax_hal::time::set_oneshot_timer(deadline_ns);
    }

    fn register_timer_callback() {}
}
