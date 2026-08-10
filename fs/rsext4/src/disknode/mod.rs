//! On-disk inode, extent, and timestamp types.
use crate::endian::*;

mod disk_format;
mod extent;
mod inode;
mod inode_flags;
mod inode_mode;
mod time;

pub use extent::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx};
pub use inode::Ext4Inode;
pub use time::{Ext4TimeSpec, Ext4Timestamp};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_gid_roundtrip_keeps_high_bits() {
        let mut inode = Ext4Inode::default();

        inode.set_uid(0x1234_5678);
        inode.set_gid(0x9abc_def0);

        assert_eq!(inode.uid(), 0x1234_5678);
        assert_eq!(inode.gid(), 0x9abc_def0);
    }

    #[test]
    fn chmod_preserves_inode_type_bits() {
        let mut inode = Ext4Inode::default();
        inode.set_mode_full(Ext4Inode::S_IFREG | Ext4Inode::S_ISUID | 0o755);

        inode.set_mode_preserve_type(0o640);

        assert_eq!(inode.i_mode & Ext4Inode::S_IFMT, Ext4Inode::S_IFREG);
        assert_eq!(inode.permissions(), 0o640);
    }

    #[test]
    fn extra_timestamp_roundtrip_works_when_extra_fields_fit() {
        let mut inode = Ext4Inode::empty_for_reuse(32);
        let ts = Ext4Timestamp::new((1_i64 << 33) + 17, 123_456_789);

        inode.set_mtime_ts(Ext4Inode::LARGE_INODE_SIZE, ts);

        assert_eq!(inode.mtime_ts(Ext4Inode::LARGE_INODE_SIZE), ts);
    }

    #[test]
    fn timestamp_without_extra_fields_clamps_to_legacy_seconds() {
        let mut inode = Ext4Inode::default();
        let ts = Ext4Timestamp::new(i32::MAX as i64 + 77, 999_999_999);

        inode.set_atime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE, ts);

        let decoded = inode.atime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE);
        assert_eq!(decoded.sec, i32::MAX as i64);
        assert_eq!(decoded.nsec, 0);
    }

    #[test]
    fn extra_isize_fit_boundaries_follow_inode_size() {
        let inode = Ext4Inode {
            i_extra_isize: 16,
            ..Default::default()
        };

        assert!(inode.field_fits(
            Ext4Inode::LARGE_INODE_SIZE,
            Ext4Inode::FIELD_END_I_MTIME_EXTRA
        ));
        assert!(!inode.field_fits(
            Ext4Inode::GOOD_OLD_INODE_SIZE,
            Ext4Inode::FIELD_END_I_CRTIME
        ));
        assert_eq!(
            Ext4Inode::required_extra_isize(Ext4Inode::FIELD_END_I_PROJID),
            32
        );
        assert_eq!(Ext4Inode::max_extra_isize(Ext4Inode::LARGE_INODE_SIZE), 128);
    }

    #[test]
    fn flag_masking_respects_inode_type_rules() {
        let dir_flags =
            Ext4Inode::EXT4_DIRSYNC_FL | Ext4Inode::EXT4_TOPDIR_FL | Ext4Inode::EXT4_NOATIME_FL;
        let reg_flags =
            Ext4Inode::EXT4_DIRSYNC_FL | Ext4Inode::EXT4_TOPDIR_FL | Ext4Inode::EXT4_NOATIME_FL;
        let symlink_flags =
            Ext4Inode::EXT4_NOATIME_FL | Ext4Inode::EXT4_NODUMP_FL | Ext4Inode::EXT4_PROJINHERIT_FL;

        assert_eq!(
            Ext4Inode::mask_flags_for_mode(Ext4Inode::S_IFDIR, dir_flags),
            dir_flags
        );
        assert_eq!(
            Ext4Inode::mask_flags_for_mode(Ext4Inode::S_IFREG, reg_flags),
            Ext4Inode::EXT4_NOATIME_FL
        );
        assert_eq!(
            Ext4Inode::mask_flags_for_mode(Ext4Inode::S_IFLNK, symlink_flags),
            Ext4Inode::EXT4_NOATIME_FL | Ext4Inode::EXT4_NODUMP_FL
        );
    }

    #[test]
    fn huge_file_inode_block_count_uses_filesystem_block_units() {
        let inode = Ext4Inode {
            i_blocks_lo: 7,
            i_flags: Ext4Inode::EXT4_HUGE_FILE_FL,
            ..Default::default()
        };

        assert_eq!(inode.blocks_count(4096, true), 56);
    }

    #[test]
    fn inode_block_count_ignores_high_fields_without_huge_file_feature() {
        let inode = Ext4Inode {
            i_blocks_lo: 7,
            l_i_blocks_high: 9,
            i_flags: Ext4Inode::EXT4_HUGE_FILE_FL,
            ..Default::default()
        };

        assert_eq!(inode.blocks_count(4096, false), 7);
    }

    #[test]
    fn inode_block_count_encoding_matches_linux_thresholds() {
        let mut inode = Ext4Inode::default();

        inode
            .set_blocks_count(u64::from(u32::MAX), 4096, true)
            .unwrap();
        assert_eq!(inode.i_blocks_lo, u32::MAX);
        assert_eq!(inode.l_i_blocks_high, 0);
        assert_eq!(inode.i_flags & Ext4Inode::EXT4_HUGE_FILE_FL, 0);

        let sectors_48 = u64::from(u32::MAX) + 1;
        inode.set_blocks_count(sectors_48, 4096, true).unwrap();
        assert_eq!(inode.blocks_count(4096, true), sectors_48);
        assert_eq!(inode.l_i_blocks_high, 1);
        assert_eq!(inode.i_flags & Ext4Inode::EXT4_HUGE_FILE_FL, 0);

        let sectors_in_fs_blocks = (1_u64 << 48) + 8;
        inode
            .set_blocks_count(sectors_in_fs_blocks, 4096, true)
            .unwrap();
        assert_ne!(inode.i_flags & Ext4Inode::EXT4_HUGE_FILE_FL, 0);
        assert_eq!(inode.blocks_count(4096, true), sectors_in_fs_blocks);

        assert!(inode.set_blocks_count(sectors_48, 4096, false).is_err());
    }

    #[test]
    fn directory_link_count_transitions_match_linux_sentinel_rules() {
        let mut inode = Ext4Inode {
            i_mode: Ext4Inode::S_IFDIR,
            i_links_count: Ext4Inode::EXT4_LINK_MAX,
            i_flags: Ext4Inode::EXT4_INDEX_FL,
            ..Default::default()
        };

        assert_eq!(inode.incremented_links_count(true).unwrap(), 1);
        assert_eq!(
            inode.incremented_links_count(false).unwrap_err().kind(),
            crate::Ext4ErrorKind::TooManyLinks
        );

        inode.i_links_count = 1;
        inode.i_flags &= !Ext4Inode::EXT4_INDEX_FL;
        assert_eq!(inode.incremented_links_count(true).unwrap(), 1);
        assert_eq!(inode.decremented_links_count().unwrap(), 1);

        inode.i_links_count = 3;
        assert_eq!(inode.decremented_links_count().unwrap(), 2);
        assert_eq!(inode.links_count_after_removing_directories(8).unwrap(), 2);

        let file = Ext4Inode {
            i_mode: Ext4Inode::S_IFREG,
            i_links_count: Ext4Inode::EXT4_LINK_MAX,
            ..Default::default()
        };
        assert_eq!(
            file.incremented_links_count(true).unwrap_err().kind(),
            crate::Ext4ErrorKind::TooManyLinks
        );
    }
}
