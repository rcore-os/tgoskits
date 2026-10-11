//! ArceOS filesystem module.
//!
//! Provides high-level filesystem operations built on top of the VFS layer,
//! including file I/O with page caching, directory traversal, and
//! `std::fs`-like APIs.

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![feature(core_io)]
#![feature(core_io_borrowed_buf)]
#![allow(clippy::new_ret_no_self)]

extern crate alloc;
#[cfg(test)]
extern crate ax_runtime;

#[macro_use]
extern crate log;

use axfs_ng_vfs::Location;
pub use axfs_ng_vfs::{VfsError, VfsResult};

pub mod api;
pub mod block;
pub mod bootargs;
pub mod bundle;
mod error;
pub mod file;
pub mod fops;
mod fs;
pub mod initramfs;
pub mod migration;
pub use fs::memory::MemoryFs;
mod fs_core;
mod highlevel;
mod mounts;
pub mod os;
pub mod root;
pub mod volume;

#[cfg(any(feature = "ext4", feature = "fat"))]
pub use block::sync_all_block_caches;
pub use block::{
    BlockRegion,
    runtime::{
        BlockBatchStats, BlockDeviceHandle, CompletionGroup, CompletionSubscription,
        block_batch_stats, block_io_stats, release_block_irqs_for_passthrough,
    },
};
pub use error::{BlockError, BlockResult};
pub(crate) use error::{block_error_to_vfs_error, io_error_to_vfs_error, vfs_error_to_io_error};
#[cfg(feature = "vfs")]
pub use highlevel::*;
pub(crate) use mounts::register_mounted_filesystem;
#[cfg(feature = "vfs")]
pub mod vfs {
    /// Create an ext4 filesystem from an owned file source and its open lease.
    #[cfg(feature = "ext4")]
    pub use crate::fs::new_from_file as new_filesystem_from_file;
    /// Create a filesystem from a native block runtime handle.
    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub use crate::fs::new_from_handle as new_filesystem_from_handle;
    pub use crate::highlevel::*;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemKind {
    Ext4,
    Fat,
}

fn finish_filesystem_init(fs: axfs_ng_vfs::Filesystem, source: &str) -> Location {
    info!("  filesystem type: {:?}", fs.name());

    // Keep an immutable namespace anchor; the actual root mount can then be
    // pivoted and detached without invalidating the namespace itself.
    let anchor = axfs_ng_vfs::Mountpoint::new_root_with_source(&MemoryFs::new(), "nullfs");
    anchor.set_readonly(true);
    let mp = anchor
        .root_location()
        .mount_with_source(&fs, source)
        .expect("initial filesystem mount");
    let root = mp.root_location();
    register_mounted_filesystem(fs);
    highlevel::ROOT_FS_CONTEXT.call_once(|| highlevel::FsContext::new(root.clone()).into_shared());
    root
}

pub fn shutdown_filesystems() -> axfs_ng_vfs::VfsResult {
    #[cfg(feature = "vfs")]
    highlevel::sync_all_cached_files(false)?;
    mounts::shutdown_registered_filesystems()
}

pub(crate) fn detect_filesystem(
    dev: &mut dyn crate::block::FsBlockDevice,
    region: BlockRegion,
) -> BlockResult<Option<FilesystemKind>> {
    #[cfg(not(any(feature = "ext4", feature = "fat")))]
    let _ = (&mut *dev, region);

    #[cfg(feature = "ext4")]
    if region_has_ext4(dev, region)? {
        return Ok(Some(FilesystemKind::Ext4));
    }

    #[cfg(feature = "fat")]
    if region_has_fat(dev, region)? {
        return Ok(Some(FilesystemKind::Fat));
    }

    Ok(None)
}

#[cfg(feature = "ext4")]
fn region_has_ext4(
    dev: &mut dyn crate::block::FsBlockDevice,
    region: BlockRegion,
) -> BlockResult<bool> {
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
fn region_has_fat(
    dev: &mut dyn crate::block::FsBlockDevice,
    region: BlockRegion,
) -> BlockResult<bool> {
    const FAT16_MAGIC: &[u8; 5] = b"FAT16";
    const FAT32_MAGIC: &[u8; 5] = b"FAT32";
    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    if visible_blocks == 0 {
        return Ok(false);
    }

    let block_size = dev.block_size();
    if block_size < 512 {
        return Ok(false);
    }

    let mut buf = alloc::vec![0u8; block_size];
    dev.read_block(start_lba, &mut buf)?;

    Ok(buf.get(510..512) == Some([0x55, 0xAA].as_slice())
        && (buf.get(54..59) == Some(FAT16_MAGIC.as_slice())
            || buf.get(82..87) == Some(FAT32_MAGIC.as_slice())))
}

#[cfg(feature = "ext4")]
fn region_has_magic_u16(
    dev: &mut dyn crate::block::FsBlockDevice,
    region: BlockRegion,
    byte_offset: usize,
    magic: u16,
) -> BlockResult<bool> {
    let block_size = dev.block_size();
    if block_size == 0 {
        return Ok(false);
    }

    let start_lba = region.start_lba;
    let visible_blocks = region.num_blocks();
    let block_index = byte_offset / block_size;
    let within_block = byte_offset % block_size;
    if visible_blocks == 0 || within_block + 2 > block_size {
        return Ok(false);
    }

    let Some(block_index_u64) = u64::try_from(block_index).ok() else {
        return Ok(false);
    };
    let Some(end_lba) = start_lba.checked_add(visible_blocks) else {
        return Ok(false);
    };
    let block_id = match start_lba.checked_add(block_index_u64) {
        Some(block_id) if block_id < end_lba => block_id,
        _ => return Ok(false),
    };

    let mut buf = alloc::vec![0u8; block_size];
    dev.read_block(block_id, &mut buf)?;

    Ok(u16::from_le_bytes([buf[within_block], buf[within_block + 1]]) == magic)
}
