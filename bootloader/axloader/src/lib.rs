#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(any(windows, unix))]
pub mod elf_image;
pub mod target;

#[cfg(any(windows, unix))]
pub use elf_image::{
    ElfImageReport, SegmentInfo, hex, inspect_elf, parse_hex_u64, validate_manifest_address,
    write_flat_binary_from_elf,
};
pub use target::{BootloaderTarget, TargetArch, known_target};
