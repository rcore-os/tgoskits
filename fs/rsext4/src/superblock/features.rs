//! Feature flags and feature tests for the ext4 superblock.

use super::Ext4Superblock;

impl Ext4Superblock {
    pub const EXT4_FEATURE_COMPAT_DIR_PREALLOC: u32 = 0x0001;
    pub const EXT4_FEATURE_COMPAT_IMAGIC_INODES: u32 = 0x0002;
    pub const EXT4_FEATURE_COMPAT_HAS_JOURNAL: u32 = 0x0004;
    pub const EXT4_FEATURE_COMPAT_EXT_ATTR: u32 = 0x0008;
    pub const EXT4_FEATURE_COMPAT_RESIZE_INODE: u32 = 0x0010;
    pub const EXT4_FEATURE_COMPAT_DIR_INDEX: u32 = 0x0020;
    pub const EXT4_FEATURE_COMPAT_LAZY_BG: u32 = 0x0040;
    pub const EXT4_FEATURE_COMPAT_EXCLUDE_INODE: u32 = 0x0080;
    pub const EXT4_FEATURE_COMPAT_EXCLUDE_BITMAP: u32 = 0x0100;
    pub const EXT4_FEATURE_COMPAT_SPARSE_SUPER2: u32 = 0x0200;
    pub const EXT4_FEATURE_COMPAT_FAST_COMMIT: u32 = 0x0400;
    pub const EXT4_FEATURE_COMPAT_ORPHAN_FILE: u32 = 0x1000;
}

impl Ext4Superblock {
    pub const EXT4_FEATURE_INCOMPAT_COMPRESSION: u32 = 0x0001;
    pub const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
    pub const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;
    pub const EXT4_FEATURE_INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
    pub const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
    pub const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
    pub const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
    pub const EXT4_FEATURE_INCOMPAT_MMP: u32 = 0x0100;
    pub const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
    pub const EXT4_FEATURE_INCOMPAT_EA_INODE: u32 = 0x0400;
    pub const EXT4_FEATURE_INCOMPAT_DIRDATA: u32 = 0x1000;
    pub const EXT4_FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;
    pub const EXT4_FEATURE_INCOMPAT_LARGEDIR: u32 = 0x4000;
    pub const EXT4_FEATURE_INCOMPAT_INLINE_DATA: u32 = 0x8000;
    pub const EXT4_FEATURE_INCOMPAT_ENCRYPT: u32 = 0x10000;
}

impl Ext4Superblock {
    pub const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
    pub const EXT4_FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0002;
    pub const EXT4_FEATURE_RO_COMPAT_BTREE_DIR: u32 = 0x0004;
    pub const EXT4_FEATURE_RO_COMPAT_HUGE_FILE: u32 = 0x0008;
    pub const EXT4_FEATURE_RO_COMPAT_GDT_CSUM: u32 = 0x0010;
    pub const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
    pub const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
    pub const EXT4_FEATURE_RO_COMPAT_HAS_SNAPSHOT: u32 = 0x0080;
    pub const EXT4_FEATURE_RO_COMPAT_QUOTA: u32 = 0x0100;
    pub const EXT4_FEATURE_RO_COMPAT_BIGALLOC: u32 = 0x0200;
    pub const EXT4_FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
    pub const EXT4_FEATURE_RO_COMPAT_REPLICA: u32 = 0x0800;
    pub const EXT4_FEATURE_RO_COMPAT_READONLY: u32 = 0x1000;
    pub const EXT4_FEATURE_RO_COMPAT_PROJECT: u32 = 0x2000;
    pub const EXT4_FEATURE_RO_COMPAT_VERITY: u32 = 0x8000;
    pub const EXT4_FEATURE_RO_COMPAT_ORPHAN_PRESENT: u32 = 0x10000;
}

impl Ext4Superblock {
    /// Returns whether a compatible feature bit is enabled.
    pub fn has_feature_compat(&self, feature: u32) -> bool {
        self.s_feature_compat & feature != 0
    }

    /// Returns whether an incompatible feature bit is enabled.
    pub fn has_feature_incompat(&self, feature: u32) -> bool {
        self.s_feature_incompat & feature != 0
    }

    /// Returns whether a read-only compatible feature bit is enabled.
    pub fn has_feature_ro_compat(&self, feature: u32) -> bool {
        self.s_feature_ro_compat & feature != 0
    }

    /// Returns whether the extent feature is enabled.
    pub fn has_extents(&self) -> bool {
        self.has_feature_incompat(Self::EXT4_FEATURE_INCOMPAT_EXTENTS)
    }

    /// Returns whether the journal feature is enabled.
    pub fn has_journal(&self) -> bool {
        self.has_feature_compat(Self::EXT4_FEATURE_COMPAT_HAS_JOURNAL)
    }
}
