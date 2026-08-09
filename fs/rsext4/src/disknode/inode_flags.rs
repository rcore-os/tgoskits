use super::*;

impl Ext4Inode {
    pub const EXT4_SECRM_FL: u32 = 0x00000001; // Secure deletion request.
    pub const EXT4_UNRM_FL: u32 = 0x00000002; // Undelete support hint.
    pub const EXT4_COMPR_FL: u32 = 0x00000004; // Compressed file.
    pub const EXT4_SYNC_FL: u32 = 0x00000008; // Synchronous updates.
    pub const EXT4_IMMUTABLE_FL: u32 = 0x00000010; // Immutable inode.
    pub const EXT4_APPEND_FL: u32 = 0x00000020; // Append-only inode.
    pub const EXT4_NODUMP_FL: u32 = 0x00000040; // Exclude from dump utilities.
    pub const EXT4_NOATIME_FL: u32 = 0x00000080; // Do not update atime.
    pub const EXT4_DIRTY_FL: u32 = 0x00000100; // Dirty compressed file.
    pub const EXT4_COMPRBLK_FL: u32 = 0x00000200; // One or more compressed clusters.
    pub const EXT4_NOCOMPR_FL: u32 = 0x00000400; // Compression disabled.
    pub const EXT4_ENCRYPT_FL: u32 = 0x00000800; // Encrypted inode.
    pub const EXT4_INDEX_FL: u32 = 0x00001000; // Hash-indexed directory.
    pub const EXT4_IMAGIC_FL: u32 = 0x00002000; // AFS directory.
    pub const EXT4_JOURNAL_DATA_FL: u32 = 0x00004000; // Data journaling enabled.
    pub const EXT4_NOTAIL_FL: u32 = 0x00008000; // Do not merge tail blocks.
    pub const EXT4_DIRSYNC_FL: u32 = 0x00010000; // Directory updates are synchronous.
    pub const EXT4_TOPDIR_FL: u32 = 0x00020000; // Top-level directory hint.
    pub const EXT4_HUGE_FILE_FL: u32 = 0x00040000; // Huge-file encoding in use.
    pub const EXT4_EXTENTS_FL: u32 = 0x00080000; // `i_block` stores an extent tree.
    pub const EXT4_EA_INODE_FL: u32 = 0x00200000; // Large xattr value inode.
    pub const EXT4_EOFBLOCKS_FL: u32 = 0x00400000; // Blocks past EOF are allocated.
    pub const EXT4_SNAPFILE_FL: u32 = 0x01000000; // Snapshot file.
    pub const EXT4_SNAPFILE_DELETED_FL: u32 = 0x04000000; // Deleted snapshot.
    pub const EXT4_SNAPFILE_SHRUNK_FL: u32 = 0x08000000; // Shrunk snapshot.
    pub const EXT4_INLINE_DATA_FL: u32 = 0x10000000; // Inline data payload.
    pub const EXT4_PROJINHERIT_FL: u32 = 0x20000000; // Inherit project ID on create.
    pub const EXT4_RESERVED_FL: u32 = 0x80000000; // Reserved internal flag bit.

    pub const EXT4_FL_USER_MODIFIABLE: u32 = Self::EXT4_SYNC_FL
        | Self::EXT4_IMMUTABLE_FL
        | Self::EXT4_APPEND_FL
        | Self::EXT4_NODUMP_FL
        | Self::EXT4_NOATIME_FL
        | Self::EXT4_DIRSYNC_FL
        | Self::EXT4_TOPDIR_FL
        | Self::EXT4_PROJINHERIT_FL;

    pub const EXT4_FL_USER_VISIBLE: u32 = Self::EXT4_FL_USER_MODIFIABLE
        | Self::EXT4_DIRTY_FL
        | Self::EXT4_COMPRBLK_FL
        | Self::EXT4_NOCOMPR_FL
        | Self::EXT4_ENCRYPT_FL
        | Self::EXT4_INDEX_FL
        | Self::EXT4_HUGE_FILE_FL
        | Self::EXT4_EXTENTS_FL
        | Self::EXT4_EA_INODE_FL
        | Self::EXT4_EOFBLOCKS_FL
        | Self::EXT4_INLINE_DATA_FL;

    pub const EXT4_FL_INHERITED: u32 = Self::EXT4_SYNC_FL
        | Self::EXT4_NODUMP_FL
        | Self::EXT4_NOATIME_FL
        | Self::EXT4_DIRSYNC_FL
        | Self::EXT4_PROJINHERIT_FL;

    pub fn mask_flags_for_mode(mode: u16, flags: u32) -> u32 {
        if mode & Self::S_IFMT == Self::S_IFDIR {
            flags
        } else if mode & Self::S_IFMT == Self::S_IFREG {
            flags & !(Self::EXT4_DIRSYNC_FL | Self::EXT4_TOPDIR_FL | Self::EXT4_PROJINHERIT_FL)
        } else {
            flags & (Self::EXT4_NODUMP_FL | Self::EXT4_NOATIME_FL)
        }
    }
}
