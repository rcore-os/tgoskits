//! Architecture-neutral guest image loading and source access.

use axvmconfig::GuestConfig;
use byte_unit::Byte;

use super::{BootImageProvider, StaticVmImage};
use crate::{AxVM, AxVmResult, GuestPhysAddr, VMMemoryRegion, ax_err, ax_err_type};

mod linux;

/// Return the q35 PCI INTx route reserved for the passthrough block device.
#[cfg(all(target_arch = "x86_64", feature = "host-fs"))]
pub(crate) const fn x86_qemu_passthrough_block_intx() -> (u8, u8, u8, usize) {
    (3, 0, 1, 19)
}

pub fn get_image_header(
    config: &GuestConfig,
    provider: &dyn BootImageProvider,
) -> Option<linux::Header> {
    match config.kernel.image_location.as_deref() {
        Some("memory") => with_memory_image(config, provider, linux::Header::parse).flatten(),
        #[cfg(any(feature = "fs", feature = "host-fs"))]
        Some("fs") => {
            let data = fs::kernel_read(config, provider, linux::Header::hdr_size()).ok()?;
            linux::Header::parse(&data)
        }
        _ => None,
    }
}

pub(crate) struct ImageLoaderCore<'a> {
    pub(crate) provider: &'a dyn BootImageProvider,
    pub(crate) main_memory: VMMemoryRegion,
    pub(crate) vm: &'a mut AxVM,
    pub(crate) config: GuestConfig,
    guest_dtb: Option<crate::boot::fdt::GuestDtbImage>,
    pub(crate) kernel_load_gpa: GuestPhysAddr,
    pub(crate) bios_load_gpa: Option<GuestPhysAddr>,
    pub(crate) ramdisk_load_gpa: Option<GuestPhysAddr>,
}

impl<'a> ImageLoaderCore<'a> {
    pub(crate) fn new(
        main_memory: VMMemoryRegion,
        config: GuestConfig,
        vm: &'a mut AxVM,
        provider: &'a dyn BootImageProvider,
        guest_dtb: Option<crate::boot::fdt::GuestDtbImage>,
    ) -> Self {
        Self {
            provider,
            main_memory,
            vm,
            config,
            guest_dtb,
            kernel_load_gpa: GuestPhysAddr::default(),
            bios_load_gpa: None,
            ramdisk_load_gpa: None,
        }
    }

    pub(crate) fn load(&mut self) -> AxVmResult {
        self.config.kernel.validate_boot_config()?;
        debug!(
            "Loading VM[{}] images into memory region: gpa={:#x}, hva={:#x}, size={:#}",
            self.vm.id(),
            self.main_memory.gpa,
            self.main_memory.hva,
            Byte::from(self.main_memory.size())
        );
        self.capture_prepared_load_addresses();

        match self.config.kernel.image_location.as_deref() {
            Some("memory") => {
                let images = memory_images_for_vm(&self.config, self.provider)?;
                crate::arch::current::load_images_from_memory(self, images)
            }
            #[cfg(any(feature = "fs", feature = "host-fs"))]
            Some("fs") => crate::arch::current::load_images_from_filesystem(self),
            _ => ax_err!(
                InvalidInput,
                "Unsupported image_location; use \"memory\" or enable fs feature for \"fs\""
            ),
        }
    }

    pub(crate) fn load_standard_images_from_memory(
        &mut self,
        images: StaticVmImage,
        load_guest_dtb: fn(&mut Self, &crate::boot::fdt::GuestDtbImage) -> AxVmResult,
    ) -> AxVmResult {
        load_vm_image_from_memory(images.kernel, self.kernel_load_gpa, &mut *self.vm)?;
        if let Some(ramdisk) = images.ramdisk {
            self.load_ramdisk_from_memory(ramdisk)?;
        }
        // Take the guest DTB out of `self` so the architecture callback can
        // mutate the owned VM while borrowing the DTB bytes independently.
        let guest_dtb = self.guest_dtb.take();
        if let Some(dtb) = guest_dtb.as_ref() {
            load_guest_dtb(self, dtb)?;
        }
        self.guest_dtb = guest_dtb;
        self.load_boot_image_from_memory(images.bios)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn load_standard_images_from_filesystem(
        &mut self,
        load_guest_dtb: fn(&mut Self, &crate::boot::fdt::GuestDtbImage) -> AxVmResult,
    ) -> AxVmResult {
        let kernel_load_gpa = self.kernel_load_gpa;
        fs::load_vm_image(
            &self.config.kernel.kernel_path,
            kernel_load_gpa,
            &mut *self.vm,
            self.provider,
        )?;
        self.load_boot_image_from_filesystem()?;
        if let Some(ramdisk_path) = self.config.kernel.ramdisk_path.clone() {
            self.load_ramdisk_from_filesystem(&ramdisk_path)?;
        }
        let guest_dtb = self.guest_dtb.take();
        if let Some(dtb) = guest_dtb.as_ref() {
            load_guest_dtb(self, dtb)?;
        }
        self.guest_dtb = guest_dtb;
        Ok(())
    }

    pub(crate) fn load_ramdisk_from_memory(&mut self, ramdisk: &[u8]) -> AxVmResult {
        let load_gpa = self.ramdisk_load_gpa()?;
        self.record_ramdisk_size(ramdisk.len());
        info!(
            "Loading ramdisk image from memory ({} bytes) into GPA @{:#x}",
            ramdisk.len(),
            load_gpa.as_usize()
        );
        load_vm_image_from_memory(ramdisk, load_gpa, &mut *self.vm)
    }

    pub(crate) fn ramdisk_load_gpa(&self) -> AxVmResult<GuestPhysAddr> {
        self.ramdisk_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "Ramdisk load addr is missed"))
    }

    fn capture_prepared_load_addresses(&mut self) {
        let (kernel_load_gpa, bios_load_gpa, ramdisk_load_gpa) = {
            let config = self.vm.config();
            (
                config.image_config.kernel_load_gpa,
                config.image_config.bios_load_gpa,
                config
                    .image_config
                    .ramdisk
                    .as_ref()
                    .map(|ramdisk| ramdisk.load_gpa),
            )
        };
        self.kernel_load_gpa = kernel_load_gpa;
        self.bios_load_gpa = bios_load_gpa;
        self.ramdisk_load_gpa = ramdisk_load_gpa;
    }

    fn load_boot_image_from_memory(&mut self, bios: Option<&[u8]>) -> AxVmResult {
        if !self.config.kernel.enable_bios {
            return Ok(());
        }
        let Some(bios) = bios else {
            return Ok(());
        };
        let load_gpa = self
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        load_vm_image_from_memory(bios, load_gpa, &mut *self.vm)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_boot_image_from_filesystem(&mut self) -> AxVmResult {
        if !self.config.kernel.enable_bios {
            return Ok(());
        }
        let Some(path) = self.config.kernel.boot_firmware_path() else {
            return Ok(());
        };
        let load_gpa = self
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        fs::load_vm_image(path, load_gpa, &mut *self.vm, self.provider)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub(crate) fn load_ramdisk_from_filesystem(&mut self, ramdisk_path: &str) -> AxVmResult {
        let load_gpa = self.ramdisk_load_gpa()?;
        let ramdisk_size = fs::image_size(ramdisk_path, self.provider)?;
        self.record_ramdisk_size(ramdisk_size);
        info!(
            "Loading ramdisk image from filesystem {} ({} bytes) into GPA @{:#x}",
            ramdisk_path,
            ramdisk_size,
            load_gpa.as_usize()
        );
        fs::load_vm_image(ramdisk_path, load_gpa, &mut *self.vm, self.provider)
    }

    fn record_ramdisk_size(&mut self, size: usize) {
        if let Some(ramdisk) = self.vm.config_mut().image_config.ramdisk.as_mut() {
            ramdisk.size = Some(size);
        }
    }
}

fn with_memory_image<F, R>(
    config: &GuestConfig,
    provider: &dyn BootImageProvider,
    func: F,
) -> Option<R>
where
    F: FnOnce(&[u8]) -> R,
{
    provider
        .static_vm_images()
        .iter()
        .find(|image| image.id == config.base.id)
        .map(|image| func(image.kernel))
}

pub(super) fn memory_images_for_vm(
    config: &GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<StaticVmImage> {
    provider
        .static_vm_images()
        .iter()
        .copied()
        .find(|image| image.id == config.base.id)
        .ok_or_else(|| {
            ax_err_type!(
                NotFound,
                "VM images are missing; pass VM configs with AXVISOR_VM_CONFIGS"
            )
        })
}

pub fn load_vm_image_from_memory(
    image_buffer: &[u8],
    load_addr: GuestPhysAddr,
    vm: &mut AxVM,
) -> AxVmResult {
    // The owning VM copies the bytes into guest memory and makes them visible to
    // the guest; no escaping guest-memory reference leaves this call.
    vm.write_to_guest(load_addr, image_buffer)
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
pub mod fs {
    use std::{format, vec::Vec};

    use axvmconfig::GuestConfig;

    use crate::{AxVM, AxVmResult, GuestPhysAddr, ax_err_type, boot::BootImageProvider};

    /// Bytes copied from a host file into guest memory per ranged read.
    const FS_LOAD_CHUNK_SIZE: usize = 0x40_0000;

    pub fn kernel_read(
        config: &GuestConfig,
        provider: &dyn BootImageProvider,
        read_size: usize,
    ) -> AxVmResult<Vec<u8>> {
        provider.read_file_exact(&config.kernel.kernel_path, read_size)
    }

    pub(crate) fn load_vm_image(
        image_path: &str,
        image_load_gpa: GuestPhysAddr,
        vm: &mut AxVM,
        provider: &dyn BootImageProvider,
    ) -> AxVmResult {
        let image_size = provider.file_size(image_path)?;
        let mut offset = 0;
        while offset < image_size {
            let chunk_len = (image_size - offset).min(FS_LOAD_CHUNK_SIZE);
            let data = provider.read_file_range(image_path, offset, chunk_len)?;
            if data.len() != chunk_len {
                return Err(ax_err_type!(
                    InvalidData,
                    format!("Image {image_path} returned a short ranged read")
                ));
            }
            vm.write_to_guest(
                GuestPhysAddr::from(image_load_gpa.as_usize() + offset),
                &data,
            )?;
            offset += chunk_len;
        }
        Ok(())
    }

    pub fn image_size(file_name: &str, provider: &dyn BootImageProvider) -> AxVmResult<usize> {
        provider.file_size(file_name)
    }

    pub fn read_full_image(
        file_name: &str,
        provider: &dyn BootImageProvider,
    ) -> AxVmResult<Vec<u8>> {
        provider.read_file(file_name)
    }
}
