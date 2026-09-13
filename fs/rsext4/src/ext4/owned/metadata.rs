//! Shared inode decoding and cache-only mounted metadata inspection.

use super::*;
use crate::{cache::inode_table::InodeCacheReader, superblock::Ext4Superblock};

#[derive(Clone, Copy, Debug)]
pub(super) struct InodeLayout {
    block_size: u32,
    inode_size: u16,
    huge_file: bool,
    large_directory: bool,
}

impl InodeLayout {
    pub(super) fn from_filesystem(filesystem: &Ext4FileSystem) -> Self {
        Self {
            block_size: filesystem.superblock.block_size() as u32,
            inode_size: filesystem.inode_disk_size(),
            huge_file: filesystem
                .superblock
                .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE),
            large_directory: filesystem
                .superblock
                .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR),
        }
    }

    pub(super) fn decode(self, number: InodeNumber, inode: Ext4Inode) -> Ext4Result<InodeInfo> {
        Ok(InodeInfo {
            number,
            mode: inode.i_mode,
            uid: inode.uid(),
            gid: inode.gid(),
            links: inode.i_links_count,
            size: inode.size_in_filesystem(self.large_directory),
            blocks: inode.blocks_count(self.block_size, self.huge_file),
            atime: inode.i_atime,
            ctime: inode.i_ctime,
            mtime: inode.i_mtime,
            btime: inode.i_crtime,
            change_attribute: inode.version(self.inode_size),
            project_id: inode.i_projid,
            flags: InodeFlags::from_bits_retain(inode.i_flags & Ext4Inode::EXT4_FL_USER_VISIBLE),
            device_number: inode.device_number()?,
        })
    }
}

/// Cache-only inode inspection using the mount's immutable on-disk layout.
/// This capability cannot load, mutate or map an inode's data blocks.
#[derive(Clone, Debug)]
pub struct InodeMetadataReader {
    cache: InodeCacheReader,
    layout: InodeLayout,
}

impl InodeMetadataReader {
    /// Returns complete cached metadata, or None when serialized lookup is
    /// required. The caller must retain this inode's allocation lifetime.
    /// Malformed cached device numbers return the same error as `Ext4::inode`.
    pub fn try_get(&self, number: InodeNumber) -> Ext4Result<Option<InodeInfo>> {
        self.cache
            .try_get(number)
            .map(|inode| self.layout.decode(number, inode))
            .transpose()
    }

    /// Returns the validated filesystem block size captured after mounting.
    pub fn block_size(&self) -> u32 {
        self.layout.block_size
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Exports a read-only view after mount/replay has selected the final cache.
    pub fn inode_metadata_reader(&self) -> InodeMetadataReader {
        InodeMetadataReader {
            cache: self.filesystem.inodetable_cache.reader(),
            layout: InodeLayout::from_filesystem(&self.filesystem),
        }
    }
}
