//! Public AArch64 image-loader facade preserving the DTB constructor contract.

use ax_errno::AxResult;
use axvmconfig::AxVMCrateConfig;

use crate::{
    AxVMRef, VMMemoryRegion,
    boot::{BootImageProvider, fdt::GuestDtbImage, images::ImageLoaderCore},
};

pub struct ImageLoader<'a>(ImageLoaderCore<'a>);

impl<'a> ImageLoader<'a> {
    pub fn new(
        main_memory: VMMemoryRegion,
        config: AxVMCrateConfig,
        vm: AxVMRef,
        provider: &'a dyn BootImageProvider,
        guest_dtb: Option<GuestDtbImage>,
    ) -> Self {
        Self(ImageLoaderCore::new(
            main_memory,
            config,
            vm,
            provider,
            guest_dtb,
        ))
    }

    pub fn load(&mut self) -> AxResult {
        self.0.load()
    }
}
