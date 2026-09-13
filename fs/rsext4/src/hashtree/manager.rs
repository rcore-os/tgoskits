//! Hash tree manager definition.

use crate::blockdev::BlockIo;

/// Coordinates hash tree parsing and lookup logic for a directory.
pub struct HashTreeManager {
    /// Hash seed copied from the superblock.
    pub(super) hash_seed: [u32; 4],
}

impl HashTreeManager {
    /// Creates a hash tree manager with the provided hash parameters.
    pub fn new(hash_seed: [u32; 4]) -> Self {
        Self { hash_seed }
    }

    /// Searches a directory for `target_name` using the hash tree when present.
    pub fn lookup<B: BlockIo>(
        &self,
        fs: &mut crate::ext4::Ext4FileSystem,
        block_dev: &mut crate::blockdev::Jbd2Dev<B>,
        dir_ino: crate::bmalloc::InodeNumber,
        dir_inode: &crate::disknode::Ext4Inode,
        target_name: &[u8],
    ) -> Result<crate::hashtree::HashTreeSearchResult, crate::hashtree::HashTreeError> {
        super::lookup::lookup(self, fs, block_dev, dir_ino, dir_inode, target_name)
    }
}
