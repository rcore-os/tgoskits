use axaddrspace::GuestPhysAddr;
use axerrno::AxResult;

use axvm::config::AxVMCrateConfig;

#[cfg(target_arch = "aarch64")]
use crate::utils::cache::cache_clean_invalidate_d;
use crate::vmm::VMRef;
use crate::vmm::config::config;

/// Loads the VM image files.
pub fn load_vm_images(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
    match config.kernel.image_location.as_deref() {
        Some("memory") => load_vm_images_from_memory(config, vm),
        #[cfg(feature = "fs")]
        Some("fs") => fs::load_vm_images_from_filesystem(config, vm),
        _ => unimplemented!(
            "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n NOTE: \"fs\" feature should be enabled if you want to load images from filesystem. (APP_FEATURES=fs)"
        ),
    }
}

/// Load VM images from memory
/// into the guest VM's memory space based on the VM configuration.
fn load_vm_images_from_memory(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
    info!("Loading VM[{}] images from memory", config.base.id);

    let vm_imags = config::get_memory_images()
        .iter()
        .find(|&v| v.id == config.base.id)
        .expect("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");

    load_vm_image_from_memory(vm_imags.kernel, config.kernel.kernel_load_addr, vm.clone())
        .expect("Failed to load VM images");

    // Load DTB image
    if let Some(buffer) = vm_imags.dtb {
        load_vm_image_from_memory(buffer, config.kernel.dtb_load_addr.unwrap(), vm.clone())
            .expect("Failed to load DTB images");
    }

    // Load BIOS image
    if let Some(buffer) = vm_imags.bios {
        load_vm_image_from_memory(buffer, config.kernel.bios_load_addr.unwrap(), vm.clone())
            .expect("Failed to load BIOS images");
    }
    #[cfg(target_arch = "aarch64")]
    {
        for mem_region in &config.kernel.memory_regions {
            debug!(
                "flush all guest cache GPA: 0x{:x}, Size: 0x{:x}",
                mem_region.gpa, mem_region.size
            );
            unsafe {
                cache_clean_invalidate_d(mem_region.gpa, mem_region.size);
            }
        }
    }

    Ok(())
}

fn load_vm_image_from_memory(image_buffer: &[u8], load_addr: usize, vm: VMRef) -> AxResult {
    let mut buffer_pos = 0;
    let image_load_gpa = GuestPhysAddr::from(load_addr);

    let image_size = image_buffer.len();

    debug!(
        "loading VM image from memory {:?} {}",
        image_load_gpa,
        image_buffer.len()
    );

    let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;

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

        // Update the position of the buffer.
        buffer_pos += bytes_to_write;

        // If the buffer is fully written, exit the loop.
        if buffer_pos >= image_size {
            debug!("copy size: {}", bytes_to_write);
            break;
        }
    }

    Ok(())
}

#[cfg(feature = "fs")]
mod fs {
    use alloc::string::String;

    use std::fs::File;

    use axerrno::{AxResult, ax_err, ax_err_type};

    use super::*;

    /// Loads the VM image files from the filesystem
    /// into the guest VM's memory space based on the VM configuration.
    pub(crate) fn load_vm_images_from_filesystem(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
        info!("Loading VM images from filesystem");
        // Load kernel image.
        load_vm_image(
            config.kernel.kernel_path,
            GuestPhysAddr::from(config.kernel.kernel_load_addr),
            vm.clone(),
        )?;
        // Load BIOS image if needed.
        if let Some(bios_path) = config.kernel.bios_path {
            if let Some(bios_load_addr) = config.kernel.bios_load_addr {
                load_vm_image(bios_path, GuestPhysAddr::from(bios_load_addr), vm.clone())?;
            } else {
                return ax_err!(NotFound, "BIOS load addr is missed");
            }
        };
        // Load Ramdisk image if needed.
        if let Some(ramdisk_path) = config.kernel.ramdisk_path {
            if let Some(ramdisk_load_addr) = config.kernel.ramdisk_load_addr {
                load_vm_image(
                    ramdisk_path,
                    GuestPhysAddr::from(ramdisk_load_addr),
                    vm.clone(),
                )?;
            } else {
                return ax_err!(NotFound, "Ramdisk load addr is missed");
            }
        };
        // Load DTB image if needed.
        // Todo: generate DTB file for guest VM.
        if let Some(dtb_path) = config.kernel.dtb_path {
            if let Some(dtb_load_addr) = config.kernel.dtb_load_addr {
                load_vm_image(dtb_path, GuestPhysAddr::from(dtb_load_addr), vm.clone())?;
            } else {
                return ax_err!(NotFound, "DTB load addr is missed");
            }
        };
        Ok(())
    }

    fn load_vm_image(image_path: String, image_load_gpa: GuestPhysAddr, vm: VMRef) -> AxResult {
        use std::io::{BufReader, Read};
        let (image_file, image_size) = open_image_file(image_path.as_str())?;

        let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;
        let mut file = BufReader::new(image_file);

        for buffer in image_load_regions {
            file.read_exact(buffer).map_err(|err| {
                ax_err_type!(
                    Io,
                    format!("Failed in reading from file {}, err {:?}", image_path, err)
                )
            })?
        }

        Ok(())
    }

    fn open_image_file(file_name: &str) -> AxResult<(File, usize)> {
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
