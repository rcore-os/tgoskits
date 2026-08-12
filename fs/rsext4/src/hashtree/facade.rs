//! Public hash tree entry points.

use super::{HashTreeError, HashTreeManager, HashTreeSearchResult};
use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::InodeNumber,
    disknode::Ext4Inode,
    ext4::Ext4FileSystem,
};

/// Looks up a directory entry through the hash tree path.
pub fn lookup_directory_entry<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    dir_ino: InodeNumber,
    dir_inode: &Ext4Inode,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    manager.lookup(fs, block_dev, dir_ino, dir_inode, target_name)
}
