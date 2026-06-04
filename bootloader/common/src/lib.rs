#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(any(windows, unix))]
pub mod elf_image;
pub mod manifest;
pub mod target;

#[cfg(any(windows, unix))]
pub use elf_image::{
    ElfImageReport, SegmentInfo, hex, inspect_elf, parse_hex_u64, validate_manifest_address,
    write_flat_binary_from_elf,
};
pub use manifest::{
    BootManifest, DownloadError, ManifestError, ParseNumberError, UrlError, parse_addr,
    parse_downloaded_manifest, parse_manifest, uri_from_device_path, write_sibling_manifest_url,
};
pub use target::{BootloaderTarget, TargetArch, known_target};
