use alloc::string::String;

use std::fs::File;

use axaddrspace::GuestPhysAddr;
use axerrno::{ax_err, ax_err_type, AxResult};

use axvm::config::AxVMCrateConfig;

use crate::vmm::VMRef;

use crate::vmm::config::config;
use alloc::vec::Vec;

/// Loads the VM image files.
pub fn load_vm_images(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
    match config.image_location.as_deref() {
        Some("memory") => load_vm_images_from_memory(config, vm),
        _ => load_vm_images_from_filesystem(config, vm),
    }
}

/// Loads the VM image files from the filesystem
/// into the guest VM's memory space based on the VM configuration.
fn load_vm_images_from_filesystem(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
    info!("Loading VM images from filesystem");
    // Load kernel image.
    load_vm_image(
        config.kernel_path,
        GuestPhysAddr::from(config.kernel_load_addr),
        vm.clone(),
    )?;
    // Load BIOS image if needed.
    if let Some(bios_path) = config.bios_path {
        if let Some(bios_load_addr) = config.bios_load_addr {
            load_vm_image(bios_path, GuestPhysAddr::from(bios_load_addr), vm.clone())?;
        } else {
            return ax_err!(NotFound, "BIOS load addr is missed");
        }
    };
    // Load Ramdisk image if needed.
    if let Some(ramdisk_path) = config.ramdisk_path {
        if let Some(ramdisk_load_addr) = config.ramdisk_load_addr {
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
    if let Some(dtb_path) = config.dtb_path {
        if let Some(dtb_load_addr) = config.dtb_load_addr {
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

/// Load VM images from memory
/// into the guest VM's memory space based on the VM configuration.
fn load_vm_images_from_memory(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
    info!("Loading VM images from memory");

    // Load DTB image
    if let Some(buffer) = config::get_dtb_binary() {
        load_vm_image_from_memory(
            Vec::from(buffer).as_mut_ptr(),
            config.dtb_load_addr.unwrap(),
            buffer.len(),
            vm.clone(),
        )
        .expect("Failed to load DTB images");
    }

    // Load BIOS image
    if let Some(buffer) = config::get_bios_binary() {
        load_vm_image_from_memory(
            Vec::from(buffer).as_mut_ptr(),
            config.bios_load_addr.unwrap(),
            buffer.len(),
            vm.clone(),
        )
        .expect("Failed to load BIOS images");
    }

    // Load kernel image.
    if let Some(buffer) = config::get_kernel_binary() {
        load_vm_image_from_memory(
            Vec::from(buffer).as_mut_ptr(),
            config.kernel_load_addr,
            buffer.len(),
            vm.clone(),
        )
        .expect("Failed to load VM images");
    } else {
        panic!("VM images is missed, Perhaps add `VM_CONFIGS=PATH/CONFIGS/FILE` command.");
    }

    Ok(())
}

fn load_vm_image_from_memory(
    buffer: *mut u8,
    load_addr: usize,
    image_size: usize,
    vm: VMRef,
) -> AxResult {
    let mut buffer_pos = 0;
    let image_load_gpa = GuestPhysAddr::from(load_addr);

    let image_load_regions = vm.get_image_load_region(image_load_gpa, image_size)?;

    for region in image_load_regions {
        let region_len = region.len();
        let bytes_to_write = region_len.min(image_size - buffer_pos);

        // copy data from memory
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.offset(buffer_pos.try_into().unwrap()),
                (&mut region[0]) as *mut u8,
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
