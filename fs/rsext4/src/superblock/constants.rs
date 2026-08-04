//! Associated constants for the ext4 superblock.

use super::Ext4Superblock;

impl Ext4Superblock {
    pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;
    pub const SUPERBLOCK_OFFSET: u64 = 1024;
    pub const SUPERBLOCK_SIZE: usize = 1024;
}

impl Ext4Superblock {
    pub const EXT4_VALID_FS: u16 = 0x0001;
    pub const EXT4_ERROR_FS: u16 = 0x0002;
    pub const EXT4_ORPHAN_FS: u16 = 0x0004;
}

impl Ext4Superblock {
    pub const EXT4_ERRORS_CONTINUE: u16 = 1;
    pub const EXT4_ERRORS_RO: u16 = 2;
    pub const EXT4_ERRORS_PANIC: u16 = 3;
}

impl Ext4Superblock {
    pub const EXT4_OS_LINUX: u32 = 0;
    pub const EXT4_OS_HURD: u32 = 1;
    pub const EXT4_OS_MASIX: u32 = 2;
    pub const EXT4_OS_FREEBSD: u32 = 3;
    pub const EXT4_OS_LITES: u32 = 4;
}

impl Ext4Superblock {
    pub const EXT4_GOOD_OLD_REV: u32 = 0;
    pub const EXT4_DYNAMIC_REV: u32 = 1;
}
