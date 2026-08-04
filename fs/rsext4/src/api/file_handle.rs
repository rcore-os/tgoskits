use alloc::string::String;

use crate::{bmalloc::InodeNumber, disknode::Ext4Inode};

/// Open file state tracked by the high-level API.
pub struct OpenFile {
    /// Inode number of the opened file.
    pub inode_num: InodeNumber,
    /// Canonical file path.
    pub path: String,
    /// Cached inode contents.
    pub inode: Ext4Inode,
    /// Current file offset in bytes.
    pub offset: u64,
}
