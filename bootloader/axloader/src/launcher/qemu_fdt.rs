//! Publishes the QEMU-generated DTB for the RISC-V OTA test firmware.

use core::{ffi::c_void, ptr::NonNull};

use axloader::fdt_image::ValidatedFdt;
use uefi::{
    Guid, Status,
    boot::{self, AllocateType, MemoryType},
    fs::{FileSystem, Path},
};

const FDT_GUID: Guid = uefi::guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");

pub(super) struct PublishedFdt {
    previous: *const c_void,
    allocation: NonNull<u8>,
    pages: usize,
}

pub(super) fn install() -> uefi::Result<PublishedFdt> {
    let mut filesystem = FileSystem::new(boot::get_image_file_system(boot::image_handle())?);
    let bytes = filesystem
        .read(Path::new(uefi::cstr16!("\\EFI\\BOOT\\qemu.dtb")))
        .map_err(|_| Status::LOAD_ERROR)?;
    drop(filesystem);
    let tree = ValidatedFdt::parse(&bytes).map_err(|_| Status::COMPROMISED_DATA)?;
    let bytes = tree.as_bytes();
    let pages = bytes.len().div_ceil(4096);
    let allocation = boot::allocate_pages(AllocateType::AnyPages, MemoryType::ACPI_RECLAIM, pages)?;
    let previous = uefi::system::with_config_table(|tables| {
        tables
            .iter()
            .find(|table| table.guid == FDT_GUID)
            .map_or(core::ptr::null(), |table| table.address)
    });
    // SAFETY: the allocation owns enough writable pages for the validated DTB.
    // The returned guard retains those pages throughout every child image call.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), allocation.as_ptr(), bytes.len());
        if let Err(error) = boot::install_configuration_table(&FDT_GUID, allocation.as_ptr().cast())
        {
            let _ = boot::free_pages(allocation, pages);
            return Err(error);
        }
    }
    Ok(PublishedFdt {
        previous,
        allocation,
        pages,
    })
}

impl Drop for PublishedFdt {
    fn drop(&mut self) {
        // SAFETY: returning OTA images leave Boot Services active. The prior
        // firmware table stays valid, and the allocation is ours to free once
        // either it has been replaced or its table entry has been removed.
        unsafe {
            if boot::install_configuration_table(&FDT_GUID, self.previous).is_ok() {
                let _ = boot::free_pages(self.allocation, self.pages);
            }
        }
    }
}
