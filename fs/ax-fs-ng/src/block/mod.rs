//! Filesystem adapters for the physical block service.
pub(crate) use ax_block::{BlockDevice as FsBlockDevice, block::*};

#[cfg(all(feature = "ext4", feature = "vfs"))]
pub(crate) mod file_image;
