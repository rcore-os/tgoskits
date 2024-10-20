use alloc::string::String;

use std::fs::File;

use axaddrspace::GuestPhysAddr;
use axerrno::{ax_err, ax_err_type, AxResult};

use axvm::config::AxVMCrateConfig;

use crate::vmm::VMRef;

/// Loads the VM image files from the filesystem
/// into the guest VM's memory space based on the VM configuration.
pub fn load_vm_images(config: AxVMCrateConfig, vm: VMRef) -> AxResult {
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
