//! On-disk inode, extent, and timestamp types.

use log::debug;

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
}
