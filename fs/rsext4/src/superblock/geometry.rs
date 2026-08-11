//! Geometry and validation helpers for the ext4 superblock.

use super::Ext4Superblock;
use crate::{
    checksum::{ext4_superblock_csum32, ext4_update_superblock_checksum},
    config::*,
    crc32c::ext4_superblock_has_metadata_csum,
    error::*,
};

impl Ext4Superblock {
    /// Returns whether the superblock magic is valid.
    pub fn is_valid(&self) -> bool {
        self.s_magic == Self::EXT4_SUPER_MAGIC
    }

    /// Returns the filesystem block size in bytes.
    pub fn block_size(&self) -> u64 {
        1024 << self.s_log_block_size
    }

    /// Returns a checked filesystem block size supported by this core.
    pub fn checked_block_size(&self) -> Ext4Result<u32> {
        let block_size = 1024u32
            .checked_shl(self.s_log_block_size)
            .ok_or_else(Ext4Error::bad_superblock)?;
        if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) || !block_size.is_power_of_two()
        {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:block_size"));
        }
        Ok(block_size)
    }

    /// Returns the validated allocation-cluster size in bytes.
    pub fn checked_cluster_size(&self) -> Ext4Result<u64> {
        let block_size = u64::from(self.checked_block_size()?);
        let cluster_size = 1024u64
            .checked_shl(self.s_log_cluster_size)
            .ok_or_else(|| Ext4Error::bad_superblock().with_operation("superblock:cluster_size"))?;
        let has_bigalloc = self.has_feature_ro_compat(Self::EXT4_FEATURE_RO_COMPAT_BIGALLOC);
        if cluster_size < block_size
            || !cluster_size.is_multiple_of(block_size)
            || (!has_bigalloc && cluster_size != block_size)
        {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:cluster_size"));
        }
        Ok(cluster_size)
    }

    /// Returns the 64-bit block count.
    pub fn blocks_count(&self) -> u64 {
        (self.s_blocks_count_hi as u64) << 32 | self.s_blocks_count_lo as u64
    }

    /// Returns the 64-bit free block count.
    pub fn free_blocks_count(&self) -> u64 {
        (self.s_free_blocks_count_hi as u64) << 32 | self.s_free_blocks_count_lo as u64
    }

    /// Returns the 64-bit reserved block count.
    pub fn reserved_blocks_count(&self) -> u64 {
        (self.s_r_blocks_count_hi as u64) << 32 | self.s_r_blocks_count_lo as u64
    }

    /// Returns the number of block groups.
    pub fn block_groups_count(&self) -> u32 {
        self.checked_block_groups_count().unwrap_or(0)
    }

    /// Returns the checked number of block groups described by this superblock.
    pub fn checked_block_groups_count(&self) -> Ext4Result<u32> {
        let blocks_per_group = u64::from(self.s_blocks_per_group);
        let data_blocks = self
            .blocks_count()
            .checked_sub(u64::from(self.s_first_data_block))
            .ok_or_else(Ext4Error::bad_superblock)?;
        if blocks_per_group == 0 || data_blocks == 0 {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:block_groups"));
        }
        u32::try_from(data_blocks.div_ceil(blocks_per_group))
            .map_err(|_| Ext4Error::bad_superblock().with_operation("superblock:block_groups"))
    }

    /// Returns the byte offset of the primary group descriptor table.
    pub fn primary_gdt_byte_offset(&self) -> Ext4Result<u64> {
        let block_size = u64::from(self.checked_block_size()?);
        u64::from(self.s_first_data_block)
            .checked_add(1)
            .and_then(|block| block.checked_mul(block_size))
            .ok_or_else(Ext4Error::overflow)
    }

    /// Validates the geometry fields needed before loading any group metadata.
    pub fn validate_geometry(&self) -> Ext4Result<()> {
        let block_size = self.checked_block_size()?;
        let expected_first_data_block = u32::from(block_size == 1024);
        if self.s_first_data_block != expected_first_data_block {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:first_data_block"));
        }

        let bits_per_block = block_size.checked_mul(8).ok_or_else(Ext4Error::overflow)?;
        let cluster_size = self.checked_cluster_size()?;
        let block_size_u64 = u64::from(block_size);
        let has_bigalloc = self.has_feature_ro_compat(Self::EXT4_FEATURE_RO_COMPAT_BIGALLOC);

        if self.s_blocks_per_group == 0
            || (!has_bigalloc && self.s_blocks_per_group > bits_per_block)
            || self.s_clusters_per_group == 0
            || self.s_clusters_per_group > bits_per_block
            || self.s_inodes_per_group == 0
            || self.s_inodes_per_group > bits_per_block
        {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:group_geometry"));
        }
        let cluster_ratio = cluster_size / block_size_u64;
        let blocks_from_clusters = u64::from(self.s_clusters_per_group)
            .checked_mul(cluster_ratio)
            .ok_or_else(Ext4Error::overflow)?;
        if blocks_from_clusters != u64::from(self.s_blocks_per_group) {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:cluster_geometry"));
        }

        let inode_size = if self.s_inode_size == 0 {
            GOOD_OLD_INODE_SIZE
        } else {
            self.s_inode_size
        };
        if inode_size < GOOD_OLD_INODE_SIZE
            || !inode_size.is_power_of_two()
            || u32::from(inode_size) > block_size
        {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:inode_size"));
        }

        let desc_size = self.get_desc_size();
        let has_64bit = self.has_feature_incompat(Self::EXT4_FEATURE_INCOMPAT_64BIT);
        if has_64bit
            && (!(GROUP_DESC_SIZE..=GROUP_DESC_SIZE_MAX).contains(&desc_size)
                || u32::from(desc_size) > block_size
                || !desc_size.is_power_of_two())
        {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:descriptor_size"));
        }

        self.checked_block_groups_count()?;
        Ok(())
    }

    /// Returns the block count per group.
    pub fn blocks_per_group(&self) -> u32 {
        self.s_blocks_per_group
    }

    /// Returns the inode count per group.
    pub fn inodes_per_group(&self) -> u32 {
        self.s_inodes_per_group
    }

    /// Returns the inode size.
    pub fn inode_size(&self) -> u16 {
        self.s_inode_size
    }

    /// Returns how many group descriptors fit in one block.
    pub fn descs_per_block(&self) -> u32 {
        let block_size = self.block_size() as u32;
        let desc_size = self.get_desc_size() as u32;
        block_size.checked_div(desc_size).unwrap_or(0)
    }

    /// Returns the on-disk group descriptor size in bytes.
    pub fn get_desc_size(&self) -> u16 {
        if !self.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT) {
            return GROUP_DESC_SIZE_OLD;
        }
        self.s_desc_size
    }

    /// Returns the inode table size in blocks per group.
    pub fn inode_table_blocks(&self) -> u32 {
        let block_size = self.block_size() as u32;
        let inode_size = u32::from(if self.s_inode_size == 0 {
            GOOD_OLD_INODE_SIZE
        } else {
            self.s_inode_size
        });
        let inodes_per_group = self.s_inodes_per_group;
        if block_size == 0 {
            0
        } else {
            (inodes_per_group * inode_size).div_ceil(block_size)
        }
    }

    /// Updates the on-disk superblock checksum when `metadata_csum` is enabled.
    pub fn update_checksum(&mut self) {
        if ext4_superblock_has_metadata_csum(self) {
            ext4_update_superblock_checksum(self);
        }
    }

    /// Verifies the superblock checksum when `metadata_csum` is enabled.
    pub fn verify_superblock(&self) -> Ext4Result<Self> {
        if ext4_superblock_has_metadata_csum(self) {
            let expected = ext4_superblock_csum32(self);
            if self.s_checksum != expected {
                return Err(Ext4Error::checksum());
            }
        }
        Ok(*self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_geometry(log_block_size: u32, first_data_block: u32) -> Ext4Superblock {
        let block_size = 1024u32 << log_block_size;
        Ext4Superblock {
            s_blocks_count_lo: first_data_block + block_size * 8,
            s_first_data_block: first_data_block,
            s_log_block_size: log_block_size,
            s_log_cluster_size: log_block_size,
            s_blocks_per_group: block_size * 8,
            s_clusters_per_group: block_size * 8,
            s_inodes_per_group: 1024,
            s_inode_size: 256,
            s_desc_size: GROUP_DESC_SIZE_OLD,
            ..Default::default()
        }
    }

    #[test]
    fn primary_gdt_follows_first_data_block_for_all_supported_sizes() {
        let one_k = valid_geometry(0, 1);
        let two_k = valid_geometry(1, 0);
        let four_k = valid_geometry(2, 0);

        assert_eq!(one_k.primary_gdt_byte_offset().unwrap(), 2048);
        assert_eq!(two_k.primary_gdt_byte_offset().unwrap(), 2048);
        assert_eq!(four_k.primary_gdt_byte_offset().unwrap(), 4096);
        one_k.validate_geometry().unwrap();
        two_k.validate_geometry().unwrap();
        four_k.validate_geometry().unwrap();
    }

    #[test]
    fn group_count_excludes_the_reserved_first_data_block() {
        let mut sb = valid_geometry(0, 1);
        assert_eq!(sb.checked_block_groups_count().unwrap(), 1);

        sb.s_blocks_count_lo += 1;
        assert_eq!(sb.checked_block_groups_count().unwrap(), 2);
    }

    #[test]
    fn geometry_rejects_mismatched_first_data_block() {
        let sb = valid_geometry(0, 0);
        let error = sb
            .validate_geometry()
            .expect_err("1 KiB filesystems reserve block zero");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
    }

    #[test]
    fn geometry_rejects_inconsistent_cluster_layout() {
        let mut sb = valid_geometry(2, 0);
        sb.s_log_cluster_size = 3;

        let error = sb
            .validate_geometry()
            .expect_err("non-bigalloc cluster size must equal the block size");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);

        let mut sb = valid_geometry(2, 0);
        sb.s_clusters_per_group -= 1;
        let error = sb
            .validate_geometry()
            .expect_err("block and cluster group geometry must agree");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
    }

    #[test]
    fn descriptor_size_matches_linux_64bit_negotiation() {
        let mut legacy = valid_geometry(2, 0);
        legacy.s_desc_size = 40;
        assert_eq!(legacy.get_desc_size(), GROUP_DESC_SIZE_OLD);
        legacy.validate_geometry().unwrap();

        let mut extended = valid_geometry(2, 0);
        extended.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT;
        extended.s_desc_size = 128;
        assert_eq!(extended.get_desc_size(), 128);
        extended.validate_geometry().unwrap();

        for invalid_size in [0, 40, 96] {
            extended.s_desc_size = invalid_size;
            let error = extended
                .validate_geometry()
                .expect_err("64-bit descriptors must use a supported power-of-two size");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
        }
    }
}
