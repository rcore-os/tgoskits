//! Architecture-neutral guest image loading and source access.

use std::format;

use axvmconfig::GuestConfig;
use byte_unit::Byte;

use super::BootImageProvider;
use crate::{AxVMRef, AxVmResult, GuestPhysAddr, VMMemoryRegion, ax_err, ax_err_type};

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
    let data = fs::kernel_read(config, provider, linux::Header::hdr_size()).ok()?;
    linux::Header::parse(&data)
}

pub(crate) struct ImageLoaderCore<'a> {
    pub(crate) provider: &'a dyn BootImageProvider,
    pub(crate) main_memory: VMMemoryRegion,
    pub(crate) vm: AxVMRef,
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
        vm: AxVMRef,
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

        crate::arch::current::load_images_from_filesystem(self)
    }

    pub(crate) fn load_standard_images_from_filesystem(
        &mut self,
        load_guest_dtb: fn(&Self, &crate::boot::fdt::GuestDtbImage) -> AxVmResult,
    ) -> AxVmResult {
        fs::load_vm_image(
            &self.config.kernel.kernel_path,
            self.kernel_load_gpa,
            self.vm.clone(),
            self.provider,
        )?;
        self.load_boot_image_from_filesystem()?;
        if let Some(ramdisk_path) = &self.config.kernel.ramdisk_path {
            self.load_ramdisk_from_filesystem(ramdisk_path)?;
        }
        if let Some(dtb) = self.guest_dtb.as_ref() {
            load_guest_dtb(self, dtb)?;
        }
        Ok(())
    }

    pub(crate) fn load_ramdisk_from_memory(&self, ramdisk: &[u8]) -> AxVmResult {
        let load_gpa = self.ramdisk_load_gpa()?;
        self.record_ramdisk_size(ramdisk.len());
        info!(
            "Loading ramdisk image from memory ({} bytes) into GPA @{:#x}",
            ramdisk.len(),
            load_gpa.as_usize()
        );
        load_vm_image_from_memory(ramdisk, load_gpa, self.vm.clone())
    }

    pub(crate) fn ramdisk_load_gpa(&self) -> AxVmResult<GuestPhysAddr> {
        self.ramdisk_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "Ramdisk load addr is missed"))
    }

    fn capture_prepared_load_addresses(&mut self) {
        self.vm.with_config(|config| {
            self.kernel_load_gpa = config.image_config.kernel_load_gpa;
            self.bios_load_gpa = config.image_config.bios_load_gpa;
            self.ramdisk_load_gpa = config.image_config.ramdisk.as_ref().map(|r| r.load_gpa);
        });
    }

    fn load_boot_image_from_filesystem(&self) -> AxVmResult {
        if !self.config.kernel.enable_bios {
            return Ok(());
        }
        let Some(path) = self.config.kernel.boot_firmware_path() else {
            return Ok(());
        };
        let load_gpa = self
            .bios_load_gpa
            .ok_or_else(|| ax_err_type!(NotFound, "boot firmware load address is missing"))?;
        fs::load_vm_image(path, load_gpa, self.vm.clone(), self.provider)
    }

    pub(crate) fn load_ramdisk_from_filesystem(&self, ramdisk_path: &str) -> AxVmResult {
        let load_gpa = self.ramdisk_load_gpa()?;
        let ramdisk = self.provider.read_file(ramdisk_path)?;
        info!(
            "Reading guest ramdisk {} for GPA @{:#x}",
            ramdisk_path,
            load_gpa.as_usize()
        );
        self.load_ramdisk_from_memory(&ramdisk)
    }

    fn record_ramdisk_size(&self, size: usize) {
        self.vm.with_config(|config| {
            if let Some(ramdisk) = config.image_config.ramdisk.as_mut() {
                ramdisk.size = Some(size);
            }
        });
    }
}

pub fn load_vm_image_from_memory(
    image_buffer: &[u8],
    load_addr: GuestPhysAddr,
    vm: AxVMRef,
) -> AxVmResult {
    let mut buffer_pos = 0;
    let image_size = image_buffer.len();
    let image_load_regions = vm.get_image_load_region(load_addr, image_size)?;

    for region in image_load_regions {
        let bytes_to_write = region.len().min(image_size - buffer_pos);
        // SAFETY: The destination comes from `get_image_load_region`, the source
        // contains `bytes_to_write` bytes, and guest memory cannot overlap the image.
        unsafe {
            std::ptr::copy_nonoverlapping(
                image_buffer[buffer_pos..].as_ptr(),
                region.as_mut_ptr().cast(),
                bytes_to_write,
            );
        }
        crate::arch::current::make_guest_memory_visible(
            (region.as_ptr() as usize).into(),
            bytes_to_write,
        );
        buffer_pos += bytes_to_write;
        if buffer_pos == image_size {
            break;
        }
    }

    if buffer_pos == image_size {
        Ok(())
    } else {
        ax_err!(
            InvalidData,
            format!("VM image was only partially loaded: {buffer_pos}/{image_size} bytes")
        )
    }
}

pub mod fs {
    use std::{format, vec::Vec};

    use axvmconfig::GuestConfig;

    use crate::{AxVMRef, AxVmResult, GuestPhysAddr, ax_err_type, boot::BootImageProvider};

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
        vm: AxVMRef,
        provider: &dyn BootImageProvider,
    ) -> AxVmResult {
        let image = provider.read_file(image_path)?;
        let image_load_regions = vm.get_image_load_region(image_load_gpa, image.len())?;
        let mut offset = 0;
        for buffer in image_load_regions {
            let end = offset + buffer.len();
            let data = image.get(offset..end).ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("Image {image_path} has an invalid load region layout")
                )
            })?;
            buffer.copy_from_slice(data);
            offset = end;
            crate::arch::current::make_guest_memory_visible(
                (buffer.as_ptr() as usize).into(),
                buffer.len(),
            );
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
