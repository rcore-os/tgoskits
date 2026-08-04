use super::*;

impl Ext4Inode {
    pub const S_IFMT: u16 = 0xF000; // File-type mask stored in `i_mode`.
    pub const S_IFSOCK: u16 = 0xC000; // Socket inode.
    pub const S_IFLNK: u16 = 0xA000; // Symbolic link inode.
    pub const S_IFREG: u16 = 0x8000; // Regular file inode.
    pub const S_IFBLK: u16 = 0x6000; // Block device inode.
    pub const S_IFDIR: u16 = 0x4000; // Directory inode.
    pub const S_IFCHR: u16 = 0x2000; // Character device inode.
    pub const S_IFIFO: u16 = 0x1000; // FIFO inode.
}

impl Ext4Inode {
    pub const S_ISUID: u16 = 0x0800; // Set-user-ID bit.
    pub const S_ISGID: u16 = 0x0400; // Set-group-ID bit.
    pub const S_ISVTX: u16 = 0x0200; // Sticky bit.
    pub const S_IRWXU: u16 = 0x01C0; // Owner permission mask.
    pub const S_IRUSR: u16 = 0x0100; // Owner read bit.
    pub const S_IWUSR: u16 = 0x0080; // Owner write bit.
    pub const S_IXUSR: u16 = 0x0040; // Owner execute bit.
    pub const S_IRWXG: u16 = 0x0038; // Group permission mask.
    pub const S_IRGRP: u16 = 0x0020; // Group read bit.
    pub const S_IWGRP: u16 = 0x0010; // Group write bit.
    pub const S_IXGRP: u16 = 0x0008; // Group execute bit.
    pub const S_IRWXO: u16 = 0x0007; // Other-users permission mask.
    pub const S_IROTH: u16 = 0x0004; // Other-users read bit.
    pub const S_IWOTH: u16 = 0x0002; // Other-users write bit.
    pub const S_IXOTH: u16 = 0x0001; // Other-users execute bit.
}
