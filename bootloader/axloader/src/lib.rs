#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(target_os = "uefi")]
extern crate alloc;

#[cfg(any(windows, unix))]
pub mod elf_image;

pub mod integrity;
#[cfg(target_os = "uefi")]
pub mod ota_disk;
pub mod ota_state;
pub mod smbios;

#[cfg(any(windows, unix))]
pub use elf_image::{
    ElfImageReport, SegmentInfo, hex, inspect_elf, parse_hex_u64, validate_manifest_address,
    write_flat_binary_from_elf,
};
