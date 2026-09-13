//! Borrowed immutable validation context for inode block mapping.

use super::{Ext4FileSystem, SystemZoneMap};
use crate::{disknode::Ext4Inode, superblock::Ext4Superblock};

/// Both references come from the same validated mount or its owned read
/// snapshot. Mapping algorithms need no mutable filesystem state through this
/// context; allocation and reclamation remain the mutation owner's concern.
#[derive(Clone, Copy)]
pub(crate) struct BlockMapContext<'a> {
    pub(crate) superblock: &'a Ext4Superblock,
    pub(crate) system_zones: &'a SystemZoneMap,
}

impl<'a> BlockMapContext<'a> {
    pub(crate) fn from_filesystem(filesystem: &'a Ext4FileSystem) -> Self {
        Self {
            superblock: &filesystem.superblock,
            system_zones: &filesystem.system_zones,
        }
    }

    pub(crate) fn block_size(self) -> usize {
        self.superblock.block_size() as usize
    }

    pub(crate) fn inode_size(self, inode: &Ext4Inode) -> u64 {
        inode.size_in_filesystem(
            self.superblock
                .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR),
        )
    }
}
