use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use std::fs::File;

use axerrno::{ax_err, ax_err_type, AxError, AxResult};
use memory_addr::VirtAddr;

use axvm::config::AxVMCrateConfig;
use axvm::AxVM;

use crate::hal::AxVMHalImpl;

pub fn load_vm_images(config: AxVMCrateConfig, vm: Arc<AxVM<AxVMHalImpl>>) -> AxResult {
    // Load kernel image.
    load_vm_image(
        config.kernel_path,
        VirtAddr::from(config.kernel_load_addr),
        vm.clone(),
    );
    // Load BIOS image if needed.
    if let Some(bios_path) = config.bios_path {
        if let Some(bios_load_addr) = config.bios_load_addr {
            load_vm_image(bios_path, VirtAddr::from(bios_load_addr), vm.clone());
        } else {
            return ax_err!(NotFound, "BIOS load addr is missed");
        }
    };
    // Load Ramdisk image if needed.
    if let Some(ramdisk_path) = config.ramdisk_path {
        if let Some(ramdisk_load_addr) = config.ramdisk_load_addr {
            load_vm_image(ramdisk_path, VirtAddr::from(ramdisk_load_addr), vm.clone());
        } else {
            return ax_err!(NotFound, "Ramdisk load addr is missed");
        }
    };
    // Load DTB image if needed.
    // Todo: generate DTB file for guest VM.
    if let Some(dtb_path) = config.dtb_path {
        if let Some(dtb_load_addr) = config.dtb_load_addr {
            load_vm_image(dtb_path, VirtAddr::from(dtb_load_addr), vm.clone());
        } else {
            return ax_err!(NotFound, "DTB load addr is missed");
        }
    };
    Ok(())
}

fn load_vm_image(
    image_path: String,
    image_load_gpa: VirtAddr,
    vm: Arc<AxVM<AxVMHalImpl>>,
) -> AxResult {
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
