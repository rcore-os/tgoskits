#![cfg_attr(not(any(windows, unix)), no_std)]

extern crate alloc;

#[cfg(any(windows, unix))]
pub mod elf_image;

pub mod boot_job;
pub mod efi_image;
pub mod elf;
pub mod http_control;
pub mod integrity;
pub mod ota;
pub mod serial_path;
pub mod smbios;
pub mod target;

#[cfg(any(windows, unix))]
pub use elf_image::{
    ElfImageReport, SegmentInfo, hex, inspect_elf, parse_hex_u64, validate_manifest_address,
    write_flat_binary_from_elf,
};
