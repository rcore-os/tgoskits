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
        let blocks = self.blocks_count();
        let blocks_per_group = self.s_blocks_per_group as u64;
        blocks.div_ceil(blocks_per_group) as u32
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
        let desc_size = self.s_desc_size as u32;
        block_size.checked_div(desc_size).unwrap_or(0)
    }

    /// Returns the on-disk group descriptor size in bytes.
    pub fn get_desc_size(&self) -> u16 {
        if self.s_desc_size == 0 {
            if self.has_feature_compat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT) {
                return GROUP_DESC_SIZE;
            } else {
                return GROUP_DESC_SIZE_OLD;
            }
        }
        self.s_desc_size
    }

    /// Returns the inode table size in blocks per group.
    pub fn inode_table_blocks(&self) -> u32 {
        let block_size = self.block_size() as u32;
        let inode_size = self.s_inode_size as u32;
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
        if ext4_superblock_has_metadata_csum(self)
            && self.s_checksum != ext4_superblock_csum32(self)
        {
            return Err(Ext4Error::checksum());
        }
        Ok(*self)
    }
}
