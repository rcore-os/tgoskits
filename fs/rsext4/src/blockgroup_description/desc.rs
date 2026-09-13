//! Block group descriptor definition and descriptor-local helpers.
use crate::{
    checksum::{ext4_block_bitmap_csum32, ext4_group_desc_csum16_zeroed, ext4_inode_bitmap_csum32},
    crc32c::crc32c::ext4_superblock_has_metadata_csum,
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

    pub(crate) fn update_bitmap_checksums(
        &mut self,
        superblock: &Ext4Superblock,
        block_bitmap: Option<&[u8]>,
        inode_bitmap: Option<&[u8]>,
    ) {
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
    }

    /// Updates known fields and the checksum inside a full on-disk record.
    ///
    /// Extension bytes beyond byte 64 remain unchanged and participate in the
    /// checksum, matching Linux's `s_desc_size` coverage.
    pub(crate) fn encode_with_checksum(
        &mut self,
        superblock: &Ext4Superblock,
        group_id: u32,
        raw_record: &mut [u8],
        block_bitmap: Option<&[u8]>,
        inode_bitmap: Option<&[u8]>,
    ) -> Ext4Result<()> {
        if raw_record.len() != superblock.get_desc_size() as usize {
            return Err(Ext4Error::corrupted().with_operation("group_descriptor:record_size"));
        }

        let has_group_desc_checksum = ext4_superblock_has_metadata_csum(superblock)
            || superblock.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_GDT_CSUM);
        if ext4_superblock_has_metadata_csum(superblock) {
            self.update_bitmap_checksums(superblock, block_bitmap, inode_bitmap);
        }
        if has_group_desc_checksum {
            self.bg_checksum = 0;
        }
        self.encode_checked(raw_record)?;

        if has_group_desc_checksum {
            self.bg_checksum = ext4_group_desc_csum16_zeroed(superblock, group_id, raw_record)
                .ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("group_descriptor:checksum_size")
                })?;
            self.encode_checked(raw_record)?;
        }
        Ok(())
    }

    /// Verifies the checksum over the complete on-disk descriptor record.
    pub(crate) fn verify_checksum_in_bytes(
        &self,
        superblock: &Ext4Superblock,
        group_id: u32,
        raw_record: &[u8],
    ) -> Ext4Result<()> {
        if raw_record.len() != superblock.get_desc_size() as usize {
            return Err(Ext4Error::corrupted().with_operation("group_descriptor:record_size"));
        }
        if !ext4_superblock_has_metadata_csum(superblock)
            && !superblock.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_GDT_CSUM)
        {
            return Ok(());
        }

        let expected =
            ext4_group_desc_csum16_zeroed(superblock, group_id, raw_record).ok_or_else(|| {
                Ext4Error::corrupted().with_operation("group_descriptor:checksum_size")
            })?;
        let stored = u16::from_le_bytes([
            *raw_record.get(30).ok_or_else(|| {
                Ext4Error::corrupted().with_operation("group_descriptor:checksum_size")
            })?,
            *raw_record.get(31).ok_or_else(|| {
                Ext4Error::corrupted().with_operation("group_descriptor:checksum_size")
            })?,
        ]);
        if stored != self.bg_checksum || expected != stored {
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
    pub fn block_bitmap_csum(&self, superblock: &Ext4Superblock) -> u32 {
        if superblock.get_desc_size() as usize >= Self::EXT4_DESC_SIZE_64BIT {
            (self.bg_block_bitmap_csum_hi as u32) << 16 | self.bg_block_bitmap_csum_lo as u32
        } else {
            self.bg_block_bitmap_csum_lo as u32
        }
    }

    /// Returns the 32-bit inode bitmap checksum.
    pub fn inode_bitmap_csum(&self, superblock: &Ext4Superblock) -> u32 {
        if superblock.get_desc_size() as usize >= Self::EXT4_DESC_SIZE_64BIT {
            (self.bg_inode_bitmap_csum_hi as u32) << 16 | self.bg_inode_bitmap_csum_lo as u32
        } else {
            self.bg_inode_bitmap_csum_lo as u32
        }
    }

    /// Returns whether a computed bitmap checksum matches this descriptor.
    ///
    /// ext4 stores only the low 16 bits of bitmap checksums in 32-byte group
    /// descriptors. The high checksum fields are present only in 64-byte
    /// descriptors.
    pub fn block_bitmap_csum_matches(&self, superblock: &Ext4Superblock, computed: u32) -> bool {
        Self::bitmap_csum_matches(superblock, self.block_bitmap_csum(superblock), computed)
    }

    /// Returns whether a computed inode bitmap checksum matches this descriptor.
    pub fn inode_bitmap_csum_matches(&self, superblock: &Ext4Superblock, computed: u32) -> bool {
        Self::bitmap_csum_matches(superblock, self.inode_bitmap_csum(superblock), computed)
    }

    fn bitmap_csum_matches(superblock: &Ext4Superblock, stored: u32, computed: u32) -> bool {
        if superblock.get_desc_size() <= Self::GOOD_OLD_DESC_SIZE as u16 {
            (stored as u16) == (computed as u16)
        } else {
            stored == computed
        }
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
