//! Small capability boundaries implemented by the selected guest architecture.

use crate::AxVmResult;

/// How a guest architecture integrates VM deadlines with the host timer source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VmTimerIntegration {
    /// VM deadlines directly program the host one-shot timer.
    DedicatedOneShot,
    /// The host runtime owns the one-shot timer and periodically checks VM deadlines.
    RuntimeCallback,
}

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

/// Architecture-specific host timer policy used by the ArceOS adapter.
pub(crate) trait HostTimePlatform {
    /// Selects whether AxVM or the host runtime owns hardware timer programming.
    const VM_TIMER_INTEGRATION: VmTimerIntegration = VmTimerIntegration::DedicatedOneShot;

    fn set_oneshot_timer(deadline_ns: u64) {
        if Self::VM_TIMER_INTEGRATION == VmTimerIntegration::DedicatedOneShot {
            ax_std::os::arceos::modules::ax_hal::time::set_oneshot_timer(deadline_ns);
        }
    }

    fn register_timer_callback() {
        if Self::VM_TIMER_INTEGRATION == VmTimerIntegration::RuntimeCallback {
            ax_std::os::arceos::modules::ax_task::register_timer_callback(|_| {
                crate::check_timer_events();
            });
        }
    }
}
