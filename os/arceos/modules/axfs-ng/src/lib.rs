//! ArceOS filesystem module.
//!
//! Provides high-level filesystem operations built on top of the VFS layer,
//! including file I/O with page caching, directory traversal, and
//! `std::fs`-like APIs.

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![allow(clippy::new_ret_no_self)]

extern crate alloc;

#[macro_use]
extern crate log;

use alloc::sync::Arc;

use axfs_ng_vfs::Location;

pub mod api;
pub mod block;
pub mod file;
pub mod fops;
mod fs;
mod fs_core;
mod highlevel;
pub mod lifecycle;
mod mount_runtime;
pub mod os;
pub mod root;
pub mod volume;

#[cfg(test)]
mod test_runtime;

pub use block::{BlockDevice, BlockDeviceMetadata, BlockRegion};
#[cfg(feature = "vfs")]
pub use highlevel::*;
pub use lifecycle::{
    FsFreezePermit, FsFreezeProgress, FsGeneration, FsOpenHandleLease, FsOperationLease,
    FsRemountPermit, FsRuntime, FsRuntimeError, FsRuntimeSnapshot, FsRuntimeState,
};
pub use mount_runtime::{
    FsHandoffError, MountRecipe, begin_filesystem_freeze, begin_filesystem_remount,
    cancel_filesystem_freeze, detach_filesystem, fail_filesystem_remount,
    filesystem_runtime_snapshot, mount_recipe, poll_filesystem_freeze, remount_filesystem,
};
#[cfg(feature = "vfs")]
pub mod vfs {
    /// Creates a filesystem from a synchronous block service.
    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub use crate::fs::new_from_device as new_filesystem_from_device;
    pub use crate::highlevel::*;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemKind {
    Ext4,
    Fat,
}

/// Initializes the filesystem subsystem from a runtime-selected block region.
pub(crate) fn init_filesystem(
    dev: Arc<dyn BlockDevice>,
    region: BlockRegion,
    description: &str,
) -> Location {
    info!("Initialize filesystem subsystem...");
    info!("  selected root device: {}", description);

    let recipe = MountRecipe::new(dev.clone(), region, None, description);
    let fs = fs::new_from_device(dev, region).unwrap_or_else(|err| {
        panic!(
            "failed to initialize filesystem on {}: {err:?}",
            description
        )
    });
    finish_filesystem_init(fs, recipe)
}

pub(crate) fn init_detected_filesystem(
    dev: Arc<dyn BlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
    description: &str,
) -> Location {
    info!("Initialize filesystem subsystem...");
    info!("  selected root device: {}", description);

    let recipe = MountRecipe::new(dev.clone(), region, Some(kind), description);
    let fs = fs::new_from_device_with_kind(dev, region, kind).unwrap_or_else(|err| {
        panic!(
            "failed to initialize filesystem on {}: {err:?}",
            description
        )
    });
    finish_filesystem_init(fs, recipe)
}

fn finish_filesystem_init(fs: axfs_ng_vfs::Filesystem, recipe: MountRecipe) -> Location {
    info!("  filesystem type: {:?}", fs.name());
    mount_runtime::install_initial_mount(fs, recipe)
        .expect("initial root filesystem publication must happen exactly once")
}

pub fn shutdown_filesystems() -> ax_errno::AxResult {
    #[cfg(feature = "vfs")]
    highlevel::sync_all_cached_files(false)?;
    if let Some(ctx) = highlevel::ROOT_FS_CONTEXT.snapshot() {
        ctx.root_dir().sync(false)?;
    }
    Ok(())
}

pub(crate) fn detect_filesystem(
    dev: &dyn BlockDevice,
    region: BlockRegion,
) -> Option<FilesystemKind> {
    #[cfg(not(any(feature = "ext4", feature = "fat")))]
    let _ = (dev, region);

    #[cfg(feature = "ext4")]
    if region_has_ext4(dev, region) {
        return Some(FilesystemKind::Ext4);
    }

    #[cfg(feature = "fat")]
    if region_has_fat(dev, region) {
        return Some(FilesystemKind::Fat);
    }

    None
}

#[cfg(feature = "ext4")]
fn region_has_ext4(dev: &dyn BlockDevice, region: BlockRegion) -> bool {
    const EXT4_SUPERBLOCK_OFFSET: usize = 1024;
    const EXT4_MAGIC_OFFSET: usize = 0x38;
    const EXT4_MAGIC: u16 = 0xEF53;
    region_has_magic_u16(
        dev,
        region,
        EXT4_SUPERBLOCK_OFFSET + EXT4_MAGIC_OFFSET,
        EXT4_MAGIC,
    )
}

#[cfg(feature = "fat")]
fn region_has_fat(dev: &dyn BlockDevice, region: BlockRegion) -> bool {
    const FAT16_MAGIC: &[u8; 5] = b"FAT16";
    const FAT32_MAGIC: &[u8; 5] = b"FAT32";
    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    if visible_blocks == 0 {
        return false;
    }

    let block_size = dev.metadata().block_size();
    if block_size < 512 {
        return false;
    }

    let mut buf = alloc::vec![0u8; block_size];
    if dev.read_blocks(start_lba, &mut buf).is_err() {
        return false;
    }

    buf.get(510..512) == Some([0x55, 0xAA].as_slice())
        && (buf.get(54..59) == Some(FAT16_MAGIC.as_slice())
            || buf.get(82..87) == Some(FAT32_MAGIC.as_slice()))
}

#[cfg(feature = "ext4")]
fn region_has_magic_u16(
    dev: &dyn BlockDevice,
    region: BlockRegion,
    byte_offset: usize,
    magic: u16,
) -> bool {
    let block_size = dev.metadata().block_size();
    if block_size == 0 {
        return false;
    }

    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    let block_index = byte_offset / block_size;
    let within_block = byte_offset % block_size;
    if visible_blocks == 0 || within_block + 2 > block_size {
        return false;
    }

    let Some(block_index_u64) = u64::try_from(block_index).ok() else {
        return false;
    };
    let Some(end_lba) = start_lba.checked_add(visible_blocks) else {
        return false;
    };
    let block_id = match start_lba.checked_add(block_index_u64) {
        Some(block_id) if block_id < end_lba => block_id,
        _ => return false,
    };

    let mut buf = alloc::vec![0u8; block_size];
    if dev.read_blocks(block_id, &mut buf).is_err() {
        return false;
    }

    u16::from_le_bytes([buf[within_block], buf[within_block + 1]]) == magic
}
