use alloc::string::{String, ToString};
use core::str;

use arceboot_cpio as cpio;
use ax_hal::mem::phys_to_virt;
use ax_io::{self as io};

/// Physical address of the pre-loaded ramdisk (0 = disabled).
pub static mut CPIO_BASE: usize = 0x0;
/// Length of the ramdisk archive in bytes. While scanning this is the
/// (clamped) upper bound; after [`init_ramdisk`] it is the archive's real
/// length as derived from the trailer record.
static mut CPIO_LEN: usize = 0;

fn truncated() -> io::Error {
    ax_log::error!("ramdisk archive is truncated or malformed");
    io::Error::UnexpectedEof
}

/// Maps a [`cpio::Error`] to an [`io::Error`].
fn map_cpio_error(e: cpio::Error) -> io::Error {
    match e {
        cpio::Error::Truncated => truncated(),
        cpio::Error::Invalid => io::Error::InvalidData,
    }
}

/// The enabled ramdisk archive as a byte slice, if any.
fn ramdisk_archive() -> io::Result<&'static [u8]> {
    let (base, len) = unsafe { (CPIO_BASE, CPIO_LEN) };
    if base == 0 {
        error!("Ramdisk is not enabled!");
        return Err(io::Error::Unsupported);
    }
    // Safety: the ramdisk stays mapped for the lifetime of the bootloader.
    Ok(unsafe { core::slice::from_raw_parts(phys_to_virt(base.into()).as_ptr(), len) })
}

/// Returns the current working directory as a [`String`].
pub fn current_dir() -> io::Result<String> {
    Ok("/".to_string())
}

/// Read the entire contents of a file into a bytes vector.
pub fn read(path: &str) -> io::Result<alloc::vec::Vec<u8>> {
    let archive = ramdisk_archive()?;
    match cpio::find_entry(archive, path) {
        Ok(Some(data)) => Ok(data.to_vec()),
        Ok(None) => Err(io::Error::NotFound),
        Err(e) => Err(map_cpio_error(e)),
    }
}

/// Read the entire contents of a file into a string.
pub fn read_to_string(path: &str) -> io::Result<String> {
    let archive = ramdisk_archive()?;
    match cpio::find_entry(archive, path) {
        Ok(Some(data)) => Ok(str::from_utf8(data).unwrap_or("<invalid utf8>").to_string()),
        Ok(None) => Err(io::Error::NotFound),
        Err(e) => Err(map_cpio_error(e)),
    }
}

fn ramdisk_enabled() -> bool {
    crate::config::boot::use_ramdisk()
}

/// Clamps the configured upper bound to the memory region containing the
/// ramdisk, so a wrong `AX_RAMDISK_SIZE` cannot make the archive scan walk
/// past actual RAM.
fn clamp_to_memory_region(start: usize, bound: usize) -> io::Result<usize> {
    for region in ax_hal::mem::memory_regions() {
        let region_start = region.paddr.as_usize();
        let region_end = region_start + region.size;
        if region_start <= start && start < region_end {
            return Ok(bound.min(region_end - start));
        }
    }
    error!("ramdisk start {:#x} is not inside any memory region", start);
    Err(io::Error::InvalidData)
}

/// Initializes the ramdisk from a pre-loaded archive: clamps the configured
/// bound to RAM, walks the archive to derive its real length (bounded by the
/// clamped `upper_bound`), and leaves the ramdisk disabled on any error.
///
/// Returns the archive's real length, which is what the DTB
/// `linux,initrd-end` property must be computed from.
pub fn init_ramdisk(start: usize, upper_bound: usize) -> io::Result<usize> {
    let bound = clamp_to_memory_region(start, upper_bound)?;
    unsafe {
        CPIO_BASE = start;
        CPIO_LEN = bound;
    }
    // Safety: the ramdisk is pre-loaded by the platform (or the QEMU
    // `-device loader`) into the physical range we just clamped to RAM.
    let archive =
        unsafe { core::slice::from_raw_parts(phys_to_virt(start.into()).as_ptr(), bound) };
    match cpio::walk_archive(archive, |_, _| false) {
        Ok(total) => {
            unsafe { CPIO_LEN = total };
            Ok(total)
        }
        Err(e) => {
            // Leave the ramdisk disabled: a broken archive must not be
            // readable through the EFI file path either.
            unsafe {
                CPIO_BASE = 0;
                CPIO_LEN = 0;
            }
            Err(map_cpio_error(e))
        }
    }
}

fn enable_dtb_ramdisk(addr: usize, size: usize) {
    unsafe {
        let mut parser = crate::dtb::DtbParser::new(crate::dtb::GLOBAL_NOW_DTB_ADDRESS).unwrap();

        // linux initrd
        if !parser.add_property("/chosen", "linux,initrd-start", &addr.to_ne_bytes()) {
            error!("Change linux,initrd-start failed!");
        }
        if !parser.add_property(
            "/chosen",
            "linux,initrd-end",
            &((addr + size).to_ne_bytes()),
        ) {
            error!("Change linux,initrd-end failed!");
        }

        // Save new dtb
        crate::dtb::GLOBAL_NOW_DTB_ADDRESS = parser.save_to_mem();
    }
}

pub fn check_ramdisk() {
    info!("Checking ramdisk.....");
    // Check the detailed annotations and explanations in configs/platforms/riscv64-qemu-virt.toml
    if ramdisk_enabled() {
        let start = crate::config::boot::ramdisk_start();
        let bound = crate::config::boot::ramdisk_size();

        match init_ramdisk(start, bound) {
            Ok(total) => {
                info!("Ramdisk at {:#x}, archive length: {:#x}", start, total);
                enable_dtb_ramdisk(start, total);
            }
            Err(e) => error!("Ramdisk init failed ({e:?}); ramdisk is disabled"),
        }

        // Best-effort probe of the test file; its absence is not fatal.
        match read_to_string("/test/arceboot.txt") {
            Ok(text) => info!("read test file context: {}", text),
            Err(e) => warn!("read test file failed: {:?}", e),
        }
    }
    info!("Checking for ramdisk is done!");
}
