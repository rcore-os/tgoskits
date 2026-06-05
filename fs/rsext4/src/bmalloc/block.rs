use super::*;
use crate::error::{Ext4Error, Ext4Result};

/// Result of a block allocation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAlloc {
    /// Block group index.
    pub group_idx: BGIndex,
    /// Block index inside the group.
    pub block_in_group: RelativeBN,
    /// Absolute filesystem block number.
    pub global_block: AbsoluteBN,
}

/// Allocates and frees data blocks inside ext4 block groups.
pub struct BlockAllocator {
    blocks_per_group: u32,
    first_data_block: u32,
}

impl BlockAllocator {
    /// Create a block allocator from the superblock layout.
    pub fn new(sb: &Ext4Superblock) -> Self {
        Self {
            blocks_per_group: sb.s_blocks_per_group,
            first_data_block: sb.s_first_data_block,
        }
    }

    /// Allocate one block from the given block group.
    pub fn alloc_block_in_group(
        &self,
        bitmap_data: &mut [u8],
        group_idx: BGIndex,
        group_desc: &Ext4GroupDesc,
    ) -> Ext4Result<BlockAlloc> {
        // Refuse the request early when the group has no free blocks left.
        if group_desc.free_blocks_count() == 0 {
            return Err(Ext4Error::no_space());
        }

        let mut bitmap = BlockBitmap::new(bitmap_data, self.blocks_per_group);

        // Scan the group bitmap for the first free slot.
        let block_in_group = self
            .find_free_block(&bitmap)?
            .ok_or(Ext4Error::no_space())?;

        // Mark the chosen block as allocated in the bitmap.
        bitmap
            .allocate(block_in_group)
            .map_err(super::map_bitmap_error)?;

        // Translate the group-local index into a filesystem-wide block number.
        let block_in_group = RelativeBN::new(block_in_group);
        let global_block = self.block_to_global(group_idx, block_in_group);

        Ok(BlockAlloc {
            group_idx,
            block_in_group,
            global_block,
        })
    }

    /// Allocate a contiguous range of blocks from the given block group.
    pub fn alloc_contiguous_blocks(
        &self,
        bitmap_data: &mut [u8],
        group_idx: BGIndex,
        count: u32,
    ) -> Ext4Result<BlockAlloc> {
        if count == 0 {
            return Err(Ext4Error::invalid_input());
        }

        let mut bitmap = BlockBitmap::new(bitmap_data, self.blocks_per_group);

        // Look for the first contiguous free range with the requested length.
        let block_in_group = self
            .find_contiguous_free_blocks(&bitmap, count)?
            .ok_or(Ext4Error::no_space())?;

        // Mark the full range as allocated in one pass.
        bitmap
            .allocate_range(block_in_group, count)
            .map_err(super::map_bitmap_error)?;

        let block_in_group = RelativeBN::new(block_in_group);
        let global_block = self.block_to_global(group_idx, block_in_group);

        Ok(BlockAlloc {
            group_idx,
            block_in_group,
            global_block,
        })
    }

    /// Free one block in the given block group bitmap.
    pub fn free_block(&self, bitmap_data: &mut [u8], block_in_group: RelativeBN) -> Ext4Result<()> {
        let mut bitmap = BlockBitmap::new(bitmap_data, self.blocks_per_group);
        bitmap
            .free(block_in_group.raw())
            .map_err(super::map_bitmap_error)?;
        Ok(())
    }

    /// Free a contiguous range of blocks.
    pub fn free_blocks(
        &self,
        bitmap_data: &mut [u8],
        start_block: RelativeBN,
        count: u32,
    ) -> Ext4Result<()> {
        let mut bitmap = BlockBitmap::new(bitmap_data, self.blocks_per_group);
        bitmap
            .free_range(start_block.raw(), count)
            .map_err(super::map_bitmap_error)?;
        Ok(())
    }

    /// Find the first free block in a bitmap.
    fn find_free_block(&self, bitmap: &BlockBitmap) -> Ext4Result<Option<u32>> {
        for block_idx in 0..self.blocks_per_group {
            if bitmap.is_allocated(block_idx) == Some(false) {
                return Ok(Some(block_idx));
            }
        }
        Ok(None)
    }

    /// Find the first contiguous free range with the requested length.
    fn find_contiguous_free_blocks(
        &self,
        bitmap: &BlockBitmap,
        count: u32,
    ) -> Ext4Result<Option<u32>> {
        let mut consecutive = 0u32;
        let mut start_idx = 0u32;

        for block_idx in 0..self.blocks_per_group {
            if bitmap.is_allocated(block_idx) == Some(false) {
                if consecutive == 0 {
                    start_idx = block_idx;
                }
                consecutive += 1;
                if consecutive == count {
                    return Ok(Some(start_idx));
                }
            } else {
                consecutive = 0;
            }
        }

        Ok(None)
    }

    /// Convert a group-local block index into an absolute block number.
    fn block_to_global(&self, group_idx: BGIndex, block_in_group: RelativeBN) -> AbsoluteBN {
        group_idx.absolute_block(block_in_group, self.first_data_block, self.blocks_per_group)
    }

    /// Convert an absolute block number into `(group_idx, block_in_group)`.
    pub fn global_to_group(&self, global_block: AbsoluteBN) -> Ext4Result<(BGIndex, RelativeBN)> {
        global_block.to_group(self.first_data_block, self.blocks_per_group)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_block_allocator_single() {
        let sb = Ext4Superblock {
            s_blocks_per_group: 1024,
            s_first_data_block: 0,
            ..Default::default()
        };

        let allocator = BlockAllocator::new(&sb);

        let mut bitmap_data = vec![0u8; 128]; // 1024 bits
        let gd = Ext4GroupDesc {
            bg_free_blocks_count_lo: 1024,
            ..Default::default()
        };

        let result = allocator.alloc_block_in_group(&mut bitmap_data, BGIndex::new(0), &gd);
        assert!(result.is_ok());

        let alloc = result.unwrap();
        assert_eq!(alloc.group_idx, BGIndex::new(0));
        assert_eq!(alloc.block_in_group, RelativeBN::new(0));
        assert_eq!(alloc.global_block, AbsoluteBN::new(0));
    }

    #[test]
    fn test_block_allocator_contiguous() {
        let sb = Ext4Superblock {
            s_blocks_per_group: 1024,
            s_first_data_block: 0,
            ..Default::default()
        };

        let allocator = BlockAllocator::new(&sb);

        let mut bitmap_data = vec![0u8; 128];

        let result = allocator.alloc_contiguous_blocks(&mut bitmap_data, BGIndex::new(0), 5);
        assert!(result.is_ok());

        let alloc = result.unwrap();
        assert_eq!(alloc.block_in_group, RelativeBN::new(0));
    }
}
