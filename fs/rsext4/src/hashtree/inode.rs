//! Hash tree inode convenience helpers.

use crate::disknode::Ext4Inode;

/// Extends `Ext4Inode` with hash tree convenience checks.
pub trait Ext4InodeHashTreeExt {
    /// Returns whether the inode enables hash tree indexing.
    fn is_htree_indexed(&self) -> bool;
}

impl Ext4InodeHashTreeExt for Ext4Inode {
    fn is_htree_indexed(&self) -> bool {
        self.i_flags & Self::EXT4_INDEX_FL != 0
    }
}
