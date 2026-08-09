//! Public hash tree entry points.

use super::{HashTreeError, HashTreeManager, HashTreeSearchResult};
use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    disknode::Ext4Inode,
    ext4::Ext4FileSystem,
};

/// Creates a hash tree manager configured from the mounted filesystem.
pub fn create_hash_tree_manager(fs: &Ext4FileSystem) -> HashTreeManager {
    HashTreeManager::new(
        fs.superblock.s_hash_seed,
        fs.superblock.s_def_hash_version,
        0,
    )
}

/// Looks up a directory entry through the hash tree path.
pub fn lookup_directory_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    dir_inode: &Ext4Inode,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    let manager = create_hash_tree_manager(fs);
    manager.lookup(fs, block_dev, dir_inode, target_name)
}
