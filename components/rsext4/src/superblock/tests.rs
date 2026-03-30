//! Unit tests for the ext4 superblock.

#[cfg(test)]
mod tests {
    use super::super::Ext4Superblock;
    use crate::endian::DiskFormat;

    #[test]
    fn test_superblock_disk_format_roundtrip() {
        let mut sb = Ext4Superblock::default();
        sb.s_magic = Ext4Superblock::EXT4_SUPER_MAGIC;
        sb.s_inodes_count = 1024;
        sb.s_blocks_count_lo = 32768;
        sb.s_blocks_count_hi = 0;
        sb.s_log_block_size = 2;
        sb.s_blocks_per_group = 8192;
        sb.s_inodes_per_group = 256;
        sb.s_uuid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        sb.s_hash_seed = [0x12345678, 0x9ABCDEF0, 0x11111111, 0x22222222];
        sb.s_inode_size = 256;
        sb.s_rev_level = Ext4Superblock::EXT4_DYNAMIC_REV;

        let mut bytes = [0u8; 1024];
        sb.to_disk_bytes(&mut bytes);

        assert_eq!(bytes[0x38], 0x53);
        assert_eq!(bytes[0x39], 0xEF);

        let sb2 = Ext4Superblock::from_disk_bytes(&bytes);

        assert_eq!(sb2.s_magic, Ext4Superblock::EXT4_SUPER_MAGIC);
        assert_eq!(sb2.s_inodes_count, 1024);
        assert_eq!(sb2.s_blocks_count_lo, 32768);
        assert_eq!(sb2.s_blocks_count_hi, 0);
        assert_eq!(sb2.s_log_block_size, 2);
        assert_eq!(sb2.s_blocks_per_group, 8192);
        assert_eq!(sb2.s_inodes_per_group, 256);
        assert_eq!(
            sb2.s_uuid,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            sb2.s_hash_seed,
            [0x12345678, 0x9ABCDEF0, 0x11111111, 0x22222222]
        );
        assert_eq!(sb2.s_inode_size, 256);
        assert_eq!(sb2.s_rev_level, Ext4Superblock::EXT4_DYNAMIC_REV);
        assert!(sb2.is_valid());
    }

    #[test]
    fn test_superblock_disk_size() {
        assert_eq!(Ext4Superblock::disk_size(), 1024);
    }

    #[test]
    fn test_superblock_64bit_values() {
        let mut sb = Ext4Superblock::default();
        sb.s_blocks_count_lo = 0xFFFFFFFF;
        sb.s_blocks_count_hi = 0x00000001;

        let mut bytes = [0u8; 1024];
        sb.to_disk_bytes(&mut bytes);

        let sb2 = Ext4Superblock::from_disk_bytes(&bytes);

        assert_eq!(sb2.blocks_count(), 0x1FFFFFFFF);
        assert_eq!(sb2.s_blocks_count_lo, 0xFFFFFFFF);
        assert_eq!(sb2.s_blocks_count_hi, 0x00000001);
    }
}
