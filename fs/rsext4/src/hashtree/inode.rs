//! Hash tree inode convenience helpers.

use crate::{disknode::Ext4Inode, entries::htree_dir};

/// Extends `Ext4Inode` with hash tree convenience checks.
pub trait Ext4InodeHashTreeExt {
    /// Returns whether the inode enables hash tree indexing.
    fn is_htree_indexed(&self) -> bool;

    /// Returns root-level hash tree metadata when the inode is indexed.
    fn get_htree_root_info(&self) -> Option<(u8, u8)>;
}

impl Ext4InodeHashTreeExt for Ext4Inode {
    fn is_htree_indexed(&self) -> bool {
        self.i_flags & Self::EXT4_INDEX_FL != 0
    }

    fn get_htree_root_info(&self) -> Option<(u8, u8)> {
        if !self.is_htree_indexed() {
            return None;
        }

        Some((htree_dir::calculate_hash(b"", 0, &[0; 4]) as u8, 0))
    }
}
