use axaddrspace::GuestPhysAddr;
use axerrno::AxResult;

use axvm::VMMemoryRegion;
use axvm::config::AxVMCrateConfig;
use byte_unit::Byte;

use crate::hal::CacheOp;
use crate::vmm::VMRef;
use crate::vmm::config::{config, get_vm_dtb_arc};

mod linux;

pub fn get_image_header(config: &AxVMCrateConfig) -> Option<linux::Header> {
    match config.kernel.image_location.as_deref() {
        Some("memory") => with_memory_image(config, linux::Header::parse),
        #[cfg(feature = "fs")]
        Some("fs") => {
            let read_size = linux::Header::hdr_size();
            let data = fs::kernal_read(config, read_size).ok()?;
            linux::Header::parse(&data)
        }
        _ => unimplemented!(
            "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n NOTE: \"fs\" feature should be enabled if you want to load images from filesystem. (APP_FEATURES=fs)"
        ),
    }
}

fn with_memory_image<F, R>(config: &AxVMCrateConfig, func: F) -> R
where
    F: FnOnce(&[u8]) -> R,
{
    let vm_imags = config::get_memory_images()
        .iter()
        .find(|&v| v.id == config.base.id)
        .expect("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");

    func(vm_imags.kernel)
}

pub struct ImageLoader {
    main_memory: VMMemoryRegion,
    vm: VMRef,
    config: AxVMCrateConfig,
    kernel_load_gpa: GuestPhysAddr,
    bios_load_gpa: Option<GuestPhysAddr>,
    dtb_load_gpa: Option<GuestPhysAddr>,
    ramdisk_load_gpa: Option<GuestPhysAddr>,
}

impl ImageLoader {
    pub fn new(main_memory: VMMemoryRegion, config: AxVMCrateConfig, vm: VMRef) -> Self {
        Self {
            main_memory,
            vm,
            config,
            kernel_load_gpa: GuestPhysAddr::default(),
            bios_load_gpa: None,
            dtb_load_gpa: None,
            ramdisk_load_gpa: None,
        }
    }

    pub fn load(&mut self) -> AxResult {
        info!(
            "Loading VM[{}] images into memory region: gpa={:#x}, hva={:#x}, size={:#}",
            self.vm.id(),
            self.main_memory.gpa,
            self.main_memory.hva,
            Byte::from(self.main_memory.size())
        );

        self.vm.with_config(|config| {
            self.kernel_load_gpa = config.image_config.kernel_load_gpa;
            self.dtb_load_gpa = config.image_config.dtb_load_gpa;
            self.bios_load_gpa = config.image_config.bios_load_gpa;
            self.ramdisk_load_gpa = config.image_config.ramdisk_load_gpa;
        });

        match self.config.kernel.image_location.as_deref() {
            Some("memory") => self.load_vm_images_from_memory(),
            #[cfg(feature = "fs")]
            Some("fs") => fs::load_vm_images_from_filesystem(self),
            _ => unimplemented!(
                "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n NOTE: \"fs\" feature should be enabled if you want to load images from filesystem. (APP_FEATURES=fs)"
            ),
        }
    }

    /// Load VM images from memory
    /// into the guest VM's memory space based on the VM configuration.
    fn load_vm_images_from_memory(&self) -> AxResult {
        info!("Loading VM[{}] images from memory", self.config.base.id);

        let vm_imags = config::get_memory_images()
            .iter()
            .find(|&v| v.id == self.config.base.id)
            .expect("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");

        load_vm_image_from_memory(vm_imags.kernel, self.kernel_load_gpa, self.vm.clone())
            .expect("Failed to load VM images");
        // Load DTB image
        let vm_config = axvm::config::AxVMConfig::from(self.config.clone());

        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(target_arch = "aarch64")]
            crate::vmm::fdt::update_fdt(
                core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8).unwrap(),
                _dtb_slice.len(),
                self.vm.clone(),
            );
        } else {
            if let Some(buffer) = vm_imags.dtb {
                #[cfg(target_arch = "riscv64")]
                load_vm_image_from_memory(buffer, self.dtb_load_gpa.unwrap(), self.vm.clone())
                    .expect("Failed to load DTB images");
            } else {
                info!("dtb_load_gpa not provided");
            }
        }

        // Load BIOS image
        if let Some(buffer) = vm_imags.bios {
            load_vm_image_from_memory(buffer, self.bios_load_gpa.unwrap(), self.vm.clone())
                .expect("Failed to load BIOS images");
        }

        // Load Ramdisk image
        if let Some(buffer) = vm_imags.ramdisk {
            load_vm_image_from_memory(buffer, self.ramdisk_load_gpa.unwrap(), self.vm.clone())
                .expect("Failed to load Ramdisk images");
        };

        Ok(())
    }
}

pub fn load_vm_image_from_memory(
    image_buffer: &[u8],
    load_addr: GuestPhysAddr,
    vm: VMRef,
) -> AxResult {
    let mut buffer_pos = 0;

    let image_size = image_buffer.len();

    debug!(
        "loading VM image from memory {:?} {}",
        load_addr,
        image_buffer.len()
    );

    let image_load_regions = vm.get_image_load_region(load_addr, image_size)?;

    for region in image_load_regions {
        let region_len = region.len();
        let bytes_to_write = region_len.min(image_size - buffer_pos);

        // copy data from memory
        unsafe {
            core::ptr::copy_nonoverlapping(
                image_buffer[buffer_pos..].as_ptr(),
                region.as_mut_ptr().cast(),
                bytes_to_write,
            );
        }

        crate::hal::arch::cache::dcache_range(
            CacheOp::Clean,
            (region.as_ptr() as usize).into(),
            region_len,
        );

        // Update the position of the buffer.
        buffer_pos += bytes_to_write;

        // If the buffer is fully written, exit the loop.
        if buffer_pos >= image_size {
            debug!("copy size: {bytes_to_write}");
            break;
        }
    }

    Ok(())
}

#[cfg(feature = "fs")]
pub mod fs {
    use super::*;
    use crate::hal::CacheOp;
    use axerrno::{AxResult, ax_err, ax_err_type};
    use std::{fs::File, vec::Vec};

    pub fn kernal_read(config: &AxVMCrateConfig, read_size: usize) -> AxResult<Vec<u8>> {
        use std::fs::File;
        use std::io::Read;
        let file_name = &config.kernel.kernel_path;

        let mut file = File::open(file_name).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to open {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;

        let mut buffer = vec![0u8; read_size];

        file.read_exact(&mut buffer).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to read {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;

        Ok(buffer)
    }

    /// Loads the VM image files from the filesystem
    /// into the guest VM's memory space based on the VM configuration.
    pub(crate) fn load_vm_images_from_filesystem(loader: &ImageLoader) -> AxResult {
        info!("Loading VM images from filesystem");
        // Load kernel image.
        load_vm_image(
            &loader.config.kernel.kernel_path,
            loader.kernel_load_gpa,
            loader.vm.clone(),
        )?;
        // Load BIOS image if needed.
        if let Some(bios_path) = &loader.config.kernel.bios_path {
            if let Some(bios_load_addr) = loader.bios_load_gpa {
                load_vm_image(bios_path, bios_load_addr, loader.vm.clone())?;
            } else {
                return ax_err!(NotFound, "BIOS load addr is missed");
            }
        };
        // Load Ramdisk image if needed.
        if let Some(ramdisk_path) = &loader.config.kernel.ramdisk_path {
            if let Some(ramdisk_load_addr) = loader.ramdisk_load_gpa {
                load_vm_image(ramdisk_path, ramdisk_load_addr, loader.vm.clone())?;
            } else {
                return ax_err!(NotFound, "Ramdisk load addr is missed");
            }
        };
        // Load DTB image if needed.
        let vm_config = axvm::config::AxVMConfig::from(loader.config.clone());
        if let Some(dtb_arc) = get_vm_dtb_arc(&vm_config) {
            let _dtb_slice: &[u8] = &dtb_arc;
            #[cfg(target_arch = "aarch64")]
            crate::vmm::fdt::update_fdt(
                core::ptr::NonNull::new(_dtb_slice.as_ptr() as *mut u8).unwrap(),
                _dtb_slice.len(),
                loader.vm.clone(),
            );
        }

        Ok(())
    }

    fn load_vm_image(image_path: &str, image_load_gpa: GuestPhysAddr, vm: VMRef) -> AxResult {
        use std::io::{BufReader, Read};
        let (image_file, image_size) = open_image_file(image_path)?;

        let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;
        let mut file = BufReader::new(image_file);

        for buffer in image_load_regions {
            file.read_exact(buffer).map_err(|err| {
                ax_err_type!(
                    Io,
                    format!("Failed in reading from file {}, err {:?}", image_path, err)
                )
            })?;

            crate::hal::arch::cache::dcache_range(
                CacheOp::Clean,
                (buffer.as_ptr() as usize).into(),
                buffer.len(),
            );
        }

        Ok(())
    }

    pub fn open_image_file(file_name: &str) -> AxResult<(File, usize)> {
        let file = File::open(file_name).map_err(|err| {
            ax_err_type!(
                NotFound,
                format!(
                    "Failed to open {}, err {:?}, please check your disk.img",
                    file_name, err
                )
            )
        })?;
        let file_size = file
            .metadata()
            .map_err(|err| {
                ax_err_type!(
                    Io,
                    format!(
                        "Failed to get metadate of file {}, err {:?}",
                        file_name, err
                    )
                )
            })?
            .size() as usize;
        Ok((file, file_size))
    }
}
