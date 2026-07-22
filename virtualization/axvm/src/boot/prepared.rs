//! Typed guest boot preparation shared by monitor integrations.

use axvmconfig::AxVMCrateConfig;

use super::{BootImageProvider, fdt::GuestDtbImage, images::ImageLoaderCore};
use crate::{AxVMRef, AxVmResult, VMMemoryRegion, config::AxVMConfig};

/// Architecture-prepared VM configuration and optional guest DTB.
#[derive(Debug)]
pub struct PreparedGuestBoot {
    config: AxVMCrateConfig,
    guest_dtb: Option<GuestDtbImage>,
}

impl PreparedGuestBoot {
    /// Loads all configured guest images into prepared VM memory.
    ///
    /// # Errors
    ///
    /// Returns an error when an image source is unavailable, an image layout is
    /// invalid, or guest memory cannot hold the configured image.
    pub fn load_images(
        self,
        main_memory: VMMemoryRegion,
        vm: AxVMRef,
        provider: &dyn BootImageProvider,
    ) -> AxVmResult {
        let mut loader =
            ImageLoaderCore::new(main_memory, self.config, vm, provider, self.guest_dtb);
        loader.load()
    }
}

/// Applies architecture boot preparation and returns a typed load request.
///
/// # Errors
///
/// Returns an error when firmware requirements are unsupported or guest boot
/// metadata cannot be parsed and validated.
pub fn prepare_guest_boot(
    vm_config: &mut AxVMConfig,
    mut config: AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<PreparedGuestBoot> {
    let guest_dtb = crate::arch::prepare_guest_boot(vm_config, &mut config, provider)?;
    Ok(PreparedGuestBoot { config, guest_dtb })
}
