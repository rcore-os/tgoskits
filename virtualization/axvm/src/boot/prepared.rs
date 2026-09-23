//! Typed guest boot preparation shared by monitor integrations.

use std::format;

use axvmconfig::GuestConfig;

use super::{BootImageProvider, fdt::GuestDtbImage, images::ImageLoaderCore};
use crate::{AxVMRef, AxVmResult, VMMemoryRegion, ax_err, config::AxVMConfig};

/// Architecture-prepared VM configuration and optional guest DTB.
#[derive(Debug)]
pub struct PreparedGuestBoot {
    config: GuestConfig,
    guest_dtb: Option<GuestDtbImage>,
}

impl PreparedGuestBoot {
    /// Returns the architecture-enriched VM configuration.
    pub const fn config(&self) -> &GuestConfig {
        &self.config
    }

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
/// Returns an error when the kernel image is empty or unavailable, firmware
/// requirements are unsupported, or guest boot metadata cannot be validated.
pub fn prepare_guest_boot(
    vm_config: &mut AxVMConfig,
    mut config: GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<PreparedGuestBoot> {
    validate_kernel_image(&config, provider)?;
    let guest_dtb = crate::arch::current::prepare_guest_boot(vm_config, &mut config, provider)?;
    Ok(PreparedGuestBoot { config, guest_dtb })
}

fn validate_kernel_image(config: &GuestConfig, provider: &dyn BootImageProvider) -> AxVmResult {
    let kernel_size = match config.kernel.image_location.as_deref() {
        Some("memory") => super::images::memory_images_for_vm(config, provider)?
            .kernel
            .len(),
        #[cfg(any(feature = "fs", feature = "host-fs"))]
        Some("fs") => provider.file_size(&config.kernel.kernel_path)?,
        _ => return Ok(()),
    };
    if kernel_size == 0 {
        return ax_err!(
            InvalidData,
            format!(
                "VM[{}] kernel image is empty: {}",
                config.base.id, config.kernel.kernel_path
            )
        );
    }
    Ok(())
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::{boot::StaticVmImage, config::AxVMConfigParams};

    struct ImageProvider(&'static [StaticVmImage]);

    impl BootImageProvider for ImageProvider {
        fn static_vm_images(&self) -> &'static [StaticVmImage] {
            self.0
        }

        #[cfg(any(feature = "fs", feature = "host-fs"))]
        fn read_file(&self, _file_name: &str) -> AxVmResult<std::vec::Vec<u8>> {
            Ok(self.0[0].kernel.to_vec())
        }
    }

    #[test]
    fn boot_preparation_rejects_empty_kernels_from_each_source() {
        static EMPTY: [StaticVmImage; 1] = [StaticVmImage {
            id: 0,
            kernel: &[],
            bios: None,
            ramdisk: None,
            dtb: None,
        }];
        static NONEMPTY: [StaticVmImage; 1] = [StaticVmImage {
            kernel: &[1],
            ..EMPTY[0]
        }];
        let sources = [
            "memory",
            #[cfg(any(feature = "fs", feature = "host-fs"))]
            "fs",
        ];
        for source in sources {
            let mut config = GuestConfig::default();
            config.kernel.image_location = Some(source.into());
            config.kernel.kernel_path = "guest-kernel.bin".into();
            let mut vm_config = AxVMConfig::new(AxVMConfigParams::default());
            let error = prepare_guest_boot(&mut vm_config, config.clone(), &ImageProvider(&EMPTY))
                .expect_err("an empty kernel must not reach architecture boot preparation");
            assert!(
                error
                    .to_string()
                    .contains("kernel image is empty: guest-kernel.bin")
            );
            prepare_guest_boot(&mut vm_config, config, &ImageProvider(&NONEMPTY))
                .expect("nonempty kernels remain eligible for architecture boot preparation");
        }
    }
}
