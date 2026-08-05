//! Disk serialization for the ext4 superblock.

use super::Ext4Superblock;
use crate::endian::*;

impl DiskFormat for Ext4Superblock {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let mut sb = Self::default();
        let mut offset = 0;

        macro_rules! read_u32 {
            () => {{
                let val = read_u32_le(&bytes[offset..]);
                offset += 4;
                val
            }};
        }

        macro_rules! read_u16 {
            () => {{
                let val = read_u16_le(&bytes[offset..]);
                offset += 2;
                val
            }};
        }

        macro_rules! read_u64 {
            () => {{
                let val = read_u64_le(&bytes[offset..]);
                offset += 8;
                val
            }};
        }

        macro_rules! read_u8 {
            () => {{
                let val = bytes[offset];
                offset += 1;
                val
            }};
        }

        macro_rules! read_bytes {
            ($len:expr) => {{
                let mut arr = [0u8; $len];
                arr.copy_from_slice(&bytes[offset..offset + $len]);
                offset += $len;
                arr
            }};
        }

        macro_rules! read_u32_array {
            ($len:expr) => {{
                let mut arr = [0u32; $len];
                for i in 0..$len {
                    arr[i] = read_u32!();
                }
                arr
            }};
        }

        sb.s_inodes_count = read_u32!();
        sb.s_blocks_count_lo = read_u32!();
        sb.s_r_blocks_count_lo = read_u32!();
        sb.s_free_blocks_count_lo = read_u32!();
        sb.s_free_inodes_count = read_u32!();
        sb.s_first_data_block = read_u32!();
        sb.s_log_block_size = read_u32!();
        sb.s_log_cluster_size = read_u32!();
        sb.s_blocks_per_group = read_u32!();
        sb.s_clusters_per_group = read_u32!();
        sb.s_inodes_per_group = read_u32!();
        sb.s_mtime = read_u32!();
        sb.s_wtime = read_u32!();

        sb.s_mnt_count = read_u16!();
        sb.s_max_mnt_count = read_u16!();
        sb.s_magic = read_u16!();
        sb.s_state = read_u16!();
        sb.s_errors = read_u16!();
        sb.s_minor_rev_level = read_u16!();
        sb.s_lastcheck = read_u32!();
        sb.s_checkinterval = read_u32!();
        sb.s_creator_os = read_u32!();
        sb.s_rev_level = read_u32!();
        sb.s_def_resuid = read_u16!();
        sb.s_def_resgid = read_u16!();

        sb.s_first_ino = read_u32!();
        sb.s_inode_size = read_u16!();
        sb.s_block_group_nr = read_u16!();
        sb.s_feature_compat = read_u32!();
        sb.s_feature_incompat = read_u32!();
        sb.s_feature_ro_compat = read_u32!();
        sb.s_uuid = read_bytes!(16);
        sb.s_volume_name = read_bytes!(16);
        sb.s_last_mounted = read_bytes!(64);
        sb.s_algorithm_usage_bitmap = read_u32!();

        sb.s_prealloc_blocks = read_u8!();
        sb.s_prealloc_dir_blocks = read_u8!();
        sb.s_reserved_gdt_blocks = read_u16!();

        sb.s_journal_uuid = read_bytes!(16);
        sb.s_journal_inum = read_u32!();
        sb.s_journal_dev = read_u32!();
        sb.s_last_orphan = read_u32!();
        sb.s_hash_seed = read_u32_array!(4);
        sb.s_def_hash_version = read_u8!();
        sb.s_jnl_backup_type = read_u8!();
        sb.s_desc_size = read_u16!();
        sb.s_default_mount_opts = read_u32!();
        sb.s_first_meta_bg = read_u32!();

        sb.s_mkfs_time = read_u32!();
        sb.s_jnl_blocks = read_u32_array!(17);

        sb.s_blocks_count_hi = read_u32!();
        sb.s_r_blocks_count_hi = read_u32!();
        sb.s_free_blocks_count_hi = read_u32!();
        sb.s_min_extra_isize = read_u16!();
        sb.s_want_extra_isize = read_u16!();
        sb.s_flags = read_u32!();
        sb.s_raid_stride = read_u16!();
        sb.s_mmp_interval = read_u16!();
        sb.s_mmp_block = read_u64!();
        sb.s_raid_stripe_width = read_u32!();

        sb.s_log_groups_per_flex = read_u8!();
        sb.s_checksum_type = read_u8!();
        sb.s_encryption_level = read_u8!();
        sb.s_reserved_pad = read_u8!();
        sb.s_kbytes_written = read_u64!();
        sb.s_snapshot_inum = read_u32!();
        sb.s_snapshot_id = read_u32!();
        sb.s_snapshot_r_blocks_count = read_u64!();
        sb.s_snapshot_list = read_u32!();

        sb.s_error_count = read_u32!();
        sb.s_first_error_time = read_u32!();
        sb.s_first_error_ino = read_u32!();
        sb.s_first_error_block = read_u64!();
        sb.s_first_error_func = read_bytes!(32);
        sb.s_first_error_line = read_u32!();
        sb.s_last_error_time = read_u32!();
        sb.s_last_error_ino = read_u32!();
        sb.s_last_error_line = read_u32!();
        sb.s_last_error_block = read_u64!();
        sb.s_last_error_func = read_bytes!(32);

        sb.s_mount_opts = read_bytes!(64);

        sb.s_usr_quota_inum = read_u32!();
        sb.s_grp_quota_inum = read_u32!();
        sb.s_overhead_blocks = read_u32!();
        sb.s_backup_bgs = [read_u32!(), read_u32!()];

        sb.s_encrypt_algos = read_bytes!(4);
        sb.s_encrypt_pw_salt = read_bytes!(16);

        sb.s_lpf_ino = read_u32!();
        sb.s_prj_quota_inum = read_u32!();
        sb.s_checksum_seed = read_u32!();
        sb.s_wtime_hi = read_u8!();
        sb.s_mtime_hi = read_u8!();
        sb.s_mkfs_time_hi = read_u8!();
        sb.s_lastcheck_hi = read_u8!();
        sb.s_first_error_time_hi = read_u8!();
        sb.s_last_error_time_hi = read_u8!();
        sb.s_first_error_errcode = read_u8!();
        sb.s_last_error_errcode = read_u8!();
        sb.s_encoding = read_u16!();
        sb.s_encoding_flags = read_u16!();
        sb.s_orphan_file_inum = read_u32!();

        sb.s_reserved = read_u32_array!(94);
        sb.s_checksum = read_u32!();

        let _ = offset;

        sb
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        let mut offset = 0;

        macro_rules! write_u32 {
            ($val:expr) => {{
                write_u32_le($val, &mut bytes[offset..]);
                offset += 4;
            }};
        }

        macro_rules! write_u16 {
            ($val:expr) => {{
                write_u16_le($val, &mut bytes[offset..]);
                offset += 2;
            }};
        }

        macro_rules! write_u64 {
            ($val:expr) => {{
                write_u64_le($val, &mut bytes[offset..]);
                offset += 8;
            }};
        }

        macro_rules! write_u8 {
            ($val:expr) => {{
                bytes[offset] = $val;
                offset += 1;
            }};
        }

        macro_rules! write_bytes {
            ($arr:expr) => {{
                let len = $arr.len();
                bytes[offset..offset + len].copy_from_slice(&$arr);
                offset += len;
            }};
        }

        macro_rules! write_u32_array {
            ($arr:expr) => {{
                for val in $arr.iter() {
                    write_u32!(*val);
                }
            }};
        }

        write_u32!(self.s_inodes_count);
        write_u32!(self.s_blocks_count_lo);
        write_u32!(self.s_r_blocks_count_lo);
        write_u32!(self.s_free_blocks_count_lo);
        write_u32!(self.s_free_inodes_count);
        write_u32!(self.s_first_data_block);
        write_u32!(self.s_log_block_size);
        write_u32!(self.s_log_cluster_size);
        write_u32!(self.s_blocks_per_group);
        write_u32!(self.s_clusters_per_group);
        write_u32!(self.s_inodes_per_group);
        write_u32!(self.s_mtime);
        write_u32!(self.s_wtime);

        write_u16!(self.s_mnt_count);
        write_u16!(self.s_max_mnt_count);
        write_u16!(self.s_magic);
        write_u16!(self.s_state);
        write_u16!(self.s_errors);
        write_u16!(self.s_minor_rev_level);
        write_u32!(self.s_lastcheck);
        write_u32!(self.s_checkinterval);
        write_u32!(self.s_creator_os);
        write_u32!(self.s_rev_level);
        write_u16!(self.s_def_resuid);
        write_u16!(self.s_def_resgid);

        write_u32!(self.s_first_ino);
        write_u16!(self.s_inode_size);
        write_u16!(self.s_block_group_nr);
        write_u32!(self.s_feature_compat);
        write_u32!(self.s_feature_incompat);
        write_u32!(self.s_feature_ro_compat);
        write_bytes!(self.s_uuid);
        write_bytes!(self.s_volume_name);
        write_bytes!(self.s_last_mounted);
        write_u32!(self.s_algorithm_usage_bitmap);

        write_u8!(self.s_prealloc_blocks);
        write_u8!(self.s_prealloc_dir_blocks);
        write_u16!(self.s_reserved_gdt_blocks);

        write_bytes!(self.s_journal_uuid);
        write_u32!(self.s_journal_inum);
        write_u32!(self.s_journal_dev);
        write_u32!(self.s_last_orphan);
        write_u32_array!(self.s_hash_seed);
        write_u8!(self.s_def_hash_version);
        write_u8!(self.s_jnl_backup_type);
        write_u16!(self.s_desc_size);
        write_u32!(self.s_default_mount_opts);
        write_u32!(self.s_first_meta_bg);

        write_u32!(self.s_mkfs_time);
        write_u32_array!(self.s_jnl_blocks);

        write_u32!(self.s_blocks_count_hi);
        write_u32!(self.s_r_blocks_count_hi);
        write_u32!(self.s_free_blocks_count_hi);
        write_u16!(self.s_min_extra_isize);
        write_u16!(self.s_want_extra_isize);
        write_u32!(self.s_flags);
        write_u16!(self.s_raid_stride);
        write_u16!(self.s_mmp_interval);
        write_u64!(self.s_mmp_block);
        write_u32!(self.s_raid_stripe_width);

        write_u8!(self.s_log_groups_per_flex);
        write_u8!(self.s_checksum_type);
        write_u8!(self.s_encryption_level);
        write_u8!(self.s_reserved_pad);
        write_u64!(self.s_kbytes_written);
        write_u32!(self.s_snapshot_inum);
        write_u32!(self.s_snapshot_id);
        write_u64!(self.s_snapshot_r_blocks_count);
        write_u32!(self.s_snapshot_list);

        write_u32!(self.s_error_count);
        write_u32!(self.s_first_error_time);
        write_u32!(self.s_first_error_ino);
        write_u64!(self.s_first_error_block);
        write_bytes!(self.s_first_error_func);
        write_u32!(self.s_first_error_line);
        write_u32!(self.s_last_error_time);
        write_u32!(self.s_last_error_ino);
        write_u32!(self.s_last_error_line);
        write_u64!(self.s_last_error_block);
        write_bytes!(self.s_last_error_func);

        write_bytes!(self.s_mount_opts);

        write_u32!(self.s_usr_quota_inum);
        write_u32!(self.s_grp_quota_inum);
        write_u32!(self.s_overhead_blocks);
        write_u32!(self.s_backup_bgs[0]);
        write_u32!(self.s_backup_bgs[1]);

        write_bytes!(self.s_encrypt_algos);
        write_bytes!(self.s_encrypt_pw_salt);

        write_u32!(self.s_lpf_ino);
        write_u32!(self.s_prj_quota_inum);
        write_u32!(self.s_checksum_seed);
        write_u8!(self.s_wtime_hi);
        write_u8!(self.s_mtime_hi);
        write_u8!(self.s_mkfs_time_hi);
        write_u8!(self.s_lastcheck_hi);
        write_u8!(self.s_first_error_time_hi);
        write_u8!(self.s_last_error_time_hi);
        write_u8!(self.s_first_error_errcode);
        write_u8!(self.s_last_error_errcode);
        write_u16!(self.s_encoding);
        write_u16!(self.s_encoding_flags);
        write_u32!(self.s_orphan_file_inum);

        write_u32_array!(self.s_reserved);
        write_u32!(self.s_checksum);

        let _ = offset;
    }

    fn disk_size() -> usize {
        Self::SUPERBLOCK_SIZE
    }
}
