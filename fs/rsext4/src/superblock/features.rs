//! Feature flags and feature tests for the ext4 superblock.

use super::Ext4Superblock;
use crate::error::{Ext4Error, Ext4Result, FeatureSet};

// These masks describe features whose read-write semantics are implemented by
// the current core. They are deliberately narrower than Linux's masks: known
// on-disk bits must not be advertised as writable before their state
// transitions and recovery rules exist here.
const SUPPORTED_INCOMPAT_FEATURES: u32 = Ext4Superblock::EXT4_FEATURE_INCOMPAT_FILETYPE
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_EXTENTS
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_FLEX_BG
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED
    | Ext4Superblock::EXT4_FEATURE_INCOMPAT_MMP;

// Linux does not inspect or update the MMP block for a read-only mount.
const READ_ONLY_SUPPORTED_INCOMPAT_FEATURES: u32 = SUPPORTED_INCOMPAT_FEATURES;

const SUPPORTED_RO_COMPAT_FEATURES: u32 = Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_LARGE_FILE
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_GDT_CSUM
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM
    | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
const MAX_DEFAULT_DIRECTORY_HASH_VERSION: u8 = 5;

impl Ext4Superblock {
    /// Directory hashes use signed filename bytes when no per-directory override exists.
    pub const EXT4_FLAGS_SIGNED_HASH: u32 = 0x0001;
    /// Directory hashes use unsigned filename bytes when no per-directory override exists.
    pub const EXT4_FLAGS_UNSIGNED_HASH: u32 = 0x0002;

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

    pub(crate) fn unsupported_incompat_features(&self, read_only: bool) -> u32 {
        let supported = if read_only {
            READ_ONLY_SUPPORTED_INCOMPAT_FEATURES
        } else {
            SUPPORTED_INCOMPAT_FEATURES
        };
        self.s_feature_incompat & !supported
    }

    pub(crate) fn unsupported_ro_compat_features(&self) -> u32 {
        self.s_feature_ro_compat & !SUPPORTED_RO_COMPAT_FEATURES
    }

    /// Checks whether this core can safely mount the advertised feature set.
    pub(crate) fn check_features(&self, read_only: bool) -> Ext4Result<()> {
        if self.s_def_hash_version > MAX_DEFAULT_DIRECTORY_HASH_VERSION {
            return Err(
                Ext4Error::bad_superblock().with_operation("superblock:default_directory_hash")
            );
        }

        let unsupported_incompat = self.unsupported_incompat_features(read_only);
        if unsupported_incompat != 0 {
            return Err(Ext4Error::unsupported_feature(
                FeatureSet::Incompatible,
                unsupported_incompat,
            ));
        }

        let unsupported_ro_compat = self.unsupported_ro_compat_features();
        if !read_only && unsupported_ro_compat != 0 {
            return Err(Ext4Error::unsupported_feature(
                FeatureSet::ReadOnlyCompatible,
                unsupported_ro_compat,
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorContext, Ext4ErrorKind, FeatureSet};

    const UNKNOWN_HIGH_BIT: u32 = 1 << 31;

    #[test]
    fn unknown_incompat_feature_is_rejected_even_read_only() {
        let sb = Ext4Superblock {
            s_feature_incompat: UNKNOWN_HIGH_BIT,
            ..Default::default()
        };

        let err = sb.check_features(true).unwrap_err();
        assert_eq!(err.kind(), Ext4ErrorKind::UnsupportedFeature);
        assert_eq!(
            err.context(),
            Some(ErrorContext::Feature {
                set: FeatureSet::Incompatible,
                bits: UNKNOWN_HIGH_BIT,
            })
        );
    }

    #[test]
    fn invalid_default_directory_hash_is_rejected_at_mount_negotiation() {
        for invalid_version in [6, 7, u8::MAX] {
            let sb = Ext4Superblock {
                s_def_hash_version: invalid_version,
                ..Default::default()
            };

            let err = sb.check_features(true).unwrap_err();
            assert_eq!(err.kind(), Ext4ErrorKind::BadSuperblock);
            assert_eq!(
                err.context(),
                Some(ErrorContext::Operation {
                    op: "superblock:default_directory_hash",
                })
            );
        }
    }

    #[test]
    fn known_but_unimplemented_incompat_feature_is_rejected() {
        let sb = Ext4Superblock {
            s_feature_incompat: Ext4Superblock::EXT4_FEATURE_INCOMPAT_ENCRYPT,
            ..Default::default()
        };

        let err = sb.check_features(false).unwrap_err();
        assert_eq!(err.kind(), Ext4ErrorKind::UnsupportedFeature);
        assert_eq!(
            err.context(),
            Some(ErrorContext::Feature {
                set: FeatureSet::Incompatible,
                bits: Ext4Superblock::EXT4_FEATURE_INCOMPAT_ENCRYPT,
            })
        );
    }

    #[test]
    fn mmp_disk_format_is_supported_for_both_mount_modes() {
        let sb = Ext4Superblock {
            s_feature_incompat: Ext4Superblock::EXT4_FEATURE_INCOMPAT_MMP,
            ..Default::default()
        };

        sb.check_features(true)
            .expect("Linux does not start MMP protection for a read-only mount");

        sb.check_features(false)
            .expect("runtime capabilities are checked by the writable mount lifecycle");
    }

    #[test]
    fn unimplemented_ro_compat_feature_requires_read_only_mount() {
        let bits = Ext4Superblock::EXT4_FEATURE_RO_COMPAT_QUOTA | UNKNOWN_HIGH_BIT;
        let sb = Ext4Superblock {
            s_feature_ro_compat: bits,
            ..Default::default()
        };

        let err = sb.check_features(false).unwrap_err();
        assert_eq!(err.kind(), Ext4ErrorKind::UnsupportedFeature);
        assert_eq!(
            err.context(),
            Some(ErrorContext::Feature {
                set: FeatureSet::ReadOnlyCompatible,
                bits,
            })
        );
        sb.check_features(true).unwrap();
    }

    #[test]
    fn unknown_compat_feature_does_not_block_mount() {
        let sb = Ext4Superblock {
            s_feature_compat: UNKNOWN_HIGH_BIT,
            s_feature_incompat: 0,
            s_feature_ro_compat: 0,
            ..Default::default()
        };

        sb.check_features(false).unwrap();
    }

    #[test]
    fn huge_file_and_dir_nlink_are_supported_for_read_write_mounts() {
        let sb = Ext4Superblock {
            s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE
                | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK,
            ..Default::default()
        };

        sb.check_features(false).unwrap();
    }
}
