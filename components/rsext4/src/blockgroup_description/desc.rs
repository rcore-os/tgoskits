//! Block group descriptor definition and descriptor-local helpers.

use log::error;

use crate::{
    checksum::{ext4_block_bitmap_csum32, ext4_group_desc_csum16, ext4_inode_bitmap_csum32},
    crc32c::crc32c::ext4_superblock_has_metadata_csum,
    endian::DiskFormat,
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
};

/// On-disk ext4 block group descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Ext4GroupDesc {
    pub bg_block_bitmap_lo: u32,
    pub bg_inode_bitmap_lo: u32,
    pub bg_inode_table_lo: u32,
    pub bg_free_blocks_count_lo: u16,
    pub bg_free_inodes_count_lo: u16,
    pub bg_used_dirs_count_lo: u16,
    pub bg_flags: u16,
    pub bg_exclude_bitmap_lo: u32,
    pub bg_block_bitmap_csum_lo: u16,
    pub bg_inode_bitmap_csum_lo: u16,
    pub bg_itable_unused_lo: u16,
    pub bg_checksum: u16,
    pub bg_block_bitmap_hi: u32,
    pub bg_inode_bitmap_hi: u32,
    pub bg_inode_table_hi: u32,
    pub bg_free_blocks_count_hi: u16,
    pub bg_free_inodes_count_hi: u16,
    pub bg_used_dirs_count_hi: u16,
    pub bg_itable_unused_hi: u16,
    pub bg_exclude_bitmap_hi: u32,
    pub bg_block_bitmap_csum_hi: u16,
    pub bg_inode_bitmap_csum_hi: u16,
    pub bg_reserved: u32,
}

impl Ext4GroupDesc {
    /// Legacy ext2/ext3-compatible descriptor size.
    pub const GOOD_OLD_DESC_SIZE: usize = 32;

    /// 64-bit ext4 descriptor size.
    pub const EXT4_DESC_SIZE_64BIT: usize = 64;

    /// Inode table and inode bitmap are uninitialized.
    pub const EXT4_BG_INODE_UNINIT: u16 = 0x0001;

    /// Block bitmap is uninitialized.
    pub const EXT4_BG_BLOCK_UNINIT: u16 = 0x0002;

    /// Inode table has already been zeroed.
    pub const EXT4_BG_INODE_ZEROED: u16 = 0x0004;

    /// Updates the descriptor checksum and optional bitmap checksum fields.
    pub fn update_checksum(
        &mut self,
        superblock: &Ext4Superblock,
        group_id: u32,
        block_bitmap: Option<&[u8]>,
        inode_bitmap: Option<&[u8]>,
    ) {
        if !ext4_superblock_has_metadata_csum(superblock) {
            return;
        }

        if let Some(bm) = block_bitmap {
            let csum = ext4_block_bitmap_csum32(superblock, bm);
            self.bg_block_bitmap_csum_lo = (csum & 0xFFFF) as u16;
            self.bg_block_bitmap_csum_hi = ((csum >> 16) & 0xFFFF) as u16;
        }
        if let Some(bm) = inode_bitmap {
            let csum = ext4_inode_bitmap_csum32(superblock, bm);
            self.bg_inode_bitmap_csum_lo = (csum & 0xFFFF) as u16;
            self.bg_inode_bitmap_csum_hi = ((csum >> 16) & 0xFFFF) as u16;
        }

        let mut desc_for_csum = *self;
        desc_for_csum.bg_checksum = 0;

        let desc_size = superblock.get_desc_size() as usize;
        let desc_size = core::cmp::min(desc_size, Ext4GroupDesc::EXT4_DESC_SIZE_64BIT);

        let mut raw_desc_bytes = [0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
        desc_for_csum.to_disk_bytes(&mut raw_desc_bytes);
        self.bg_checksum =
            ext4_group_desc_csum16(superblock, group_id, &raw_desc_bytes[..desc_size]);
    }

    /// Verifies the descriptor checksum when `metadata_csum` is enabled.
    pub fn verify_checksum(&self, superblock: &Ext4Superblock, group_id: u32) -> Ext4Result<()> {
        if !ext4_superblock_has_metadata_csum(superblock) {
            return Ok(());
        }

        let mut desc_for_csum = *self;
        desc_for_csum.bg_checksum = 0;

        let desc_size = superblock.get_desc_size() as usize;
        let desc_size = core::cmp::min(desc_size, Ext4GroupDesc::EXT4_DESC_SIZE_64BIT);

        let mut raw_desc_bytes = [0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
        desc_for_csum.to_disk_bytes(&mut raw_desc_bytes);
        let expected = ext4_group_desc_csum16(superblock, group_id, &raw_desc_bytes[..desc_size]);
        if expected != self.bg_checksum {
            error!(
                "Group descriptor checksum mismatch for group {}: expected {:04x}, got {:04x}",
                group_id, self.bg_checksum, expected
            );
            return Err(Ext4Error::checksum());
        }
        Ok(())
    }

    /// Returns the 64-bit block bitmap block number.
    pub fn block_bitmap(&self) -> u64 {
        (self.bg_block_bitmap_hi as u64) << 32 | self.bg_block_bitmap_lo as u64
    }

    /// Returns the 64-bit inode bitmap block number.
    pub fn inode_bitmap(&self) -> u64 {
        (self.bg_inode_bitmap_hi as u64) << 32 | self.bg_inode_bitmap_lo as u64
    }

    /// Returns the 64-bit inode table start block.
    pub fn inode_table(&self) -> u64 {
        (self.bg_inode_table_hi as u64) << 32 | self.bg_inode_table_lo as u64
    }

    /// Returns the 32-bit free block count.
    pub fn free_blocks_count(&self) -> u32 {
        (self.bg_free_blocks_count_hi as u32) << 16 | self.bg_free_blocks_count_lo as u32
    }

    /// Returns the 32-bit free inode count.
    pub fn free_inodes_count(&self) -> u32 {
        (self.bg_free_inodes_count_hi as u32) << 16 | self.bg_free_inodes_count_lo as u32
    }

    /// Returns the 32-bit used directory count.
    pub fn used_dirs_count(&self) -> u32 {
        (self.bg_used_dirs_count_hi as u32) << 16 | self.bg_used_dirs_count_lo as u32
    }

    /// Returns the 32-bit unused inode table count.
    pub fn itable_unused(&self) -> u32 {
        (self.bg_itable_unused_hi as u32) << 16 | self.bg_itable_unused_lo as u32
    }

    /// Returns the 64-bit exclude bitmap block number.
    pub fn exclude_bitmap(&self) -> u64 {
        (self.bg_exclude_bitmap_hi as u64) << 32 | self.bg_exclude_bitmap_lo as u64
    }

    /// Returns the 32-bit block bitmap checksum.
    pub fn block_bitmap_csum(&self) -> u32 {
        (self.bg_block_bitmap_csum_hi as u32) << 16 | self.bg_block_bitmap_csum_lo as u32
    }

    /// Returns the 32-bit inode bitmap checksum.
    pub fn inode_bitmap_csum(&self) -> u32 {
        (self.bg_inode_bitmap_csum_hi as u32) << 16 | self.bg_inode_bitmap_csum_lo as u32
    }

    /// Returns whether the block group is marked uninitialized.
    pub fn is_uninit_bg(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_UNINIT != 0
    }

    /// Returns whether the block bitmap is marked uninitialized.
    pub fn is_block_bitmap_uninit(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_BLOCK_UNINIT != 0
    }

    /// Returns whether the inode bitmap is marked uninitialized.
    pub fn is_inode_bitmap_uninit(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_UNINIT != 0
    }

    /// Returns whether the inode table is marked zeroed.
    pub fn is_inode_table_zeroed(&self) -> bool {
        self.bg_flags & Self::EXT4_BG_INODE_ZEROED != 0
    }
}
