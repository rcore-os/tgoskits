use alloc::string::String;

use crate::BlockDevice;
use crate::Ext4FileSystem;
use crate::Ext4Result;
use crate::Jbd2Dev;
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

/// Refreshes the cached inode view using the inode number (fd-like behavior).
///
/// This avoids path-based relookup (which would diverge from Linux fd semantics
/// after rename/unlink).
pub fn refresh_open_file_inode_by_num<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
) -> Ext4Result<()> {
    // flush memory inode shnapshot from inode table cache or disk
    file.inode = fs.get_inode_by_num(dev, file.inode_num)?;
    Ok(())
}
