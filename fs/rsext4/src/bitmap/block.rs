//! Block bitmap wrappers.

use log::error;

use crate::bitmap::BitmapError;

/// Block bitmap view with allocation helpers.
#[derive(Debug)]
pub struct BlockBitmap<'a> {
    data: &'a mut [u8],
    blocks_per_group: u32,
}

impl<'a> BlockBitmap<'a> {
    /// Creates a new block bitmap view.
    pub fn new(data: &'a mut [u8], blocks_per_group: u32) -> Self {
        Self {
            data,
            blocks_per_group,
        }
    }

    /// Returns whether the block is allocated.
    pub fn is_allocated(&self, block_idx: u32) -> Option<bool> {
        if block_idx >= self.blocks_per_group {
            return None;
        }

        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return None;
        }

        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// Returns whether the block is free.
    pub fn is_free(&self, block_idx: u32) -> Option<bool> {
        self.is_allocated(block_idx).map(|allocated| !allocated)
    }

    /// Returns the first free block index in the group.
    pub fn find_first_free(&self) -> Option<u32> {
        for (byte_idx, &byte) in self.data.iter().enumerate() {
            if byte != 0xFF {
                for bit_idx in 0..8 {
                    if (byte & (1 << bit_idx)) == 0 {
                        let block_idx = (byte_idx * 8 + bit_idx) as u32;
                        if block_idx < self.blocks_per_group {
                            return Some(block_idx);
                        }
                    }
                }
            }
        }
        None
    }

    /// Returns the first contiguous free range of `count` blocks.
    pub fn find_contiguous_free(&self, count: u32) -> Option<u32> {
        if count == 0 {
            return None;
        }

        let mut consecutive = 0u32;
        let mut start_idx = 0u32;

        for block_idx in 0..self.blocks_per_group {
            if self.is_free(block_idx) == Some(true) {
                if consecutive == 0 {
                    start_idx = block_idx;
                }
                consecutive += 1;
                if consecutive == count {
                    return Some(start_idx);
                }
            } else {
                consecutive = 0;
            }
        }

        None
    }

    /// Counts free blocks in the group.
    pub fn count_free(&self) -> u32 {
        let mut count = 0u32;

        for block_idx in 0..self.blocks_per_group {
            if self.is_free(block_idx) == Some(true) {
                count += 1;
            }
        }

        count
    }

    /// Counts allocated blocks in the group.
    pub fn count_allocated(&self) -> u32 {
        self.blocks_per_group - self.count_free()
    }

    /// Marks a block as allocated.
    pub fn allocate(&mut self, block_idx: u32) -> Result<(), BitmapError> {
        if block_idx >= self.blocks_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }

        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) != 0 {
            return Err(BitmapError::AlreadyAllocated);
        }

        self.data[byte_idx] |= 1 << bit_idx;
        Ok(())
    }

    /// Marks a block as free.
    pub fn free(&mut self, block_idx: u32) -> Result<(), BitmapError> {
        if block_idx >= self.blocks_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }

        let byte_idx = (block_idx / 8) as usize;
        let bit_idx = (block_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) == 0 {
            error!("Block num:{block_idx} already free!");
            return Err(BitmapError::AlreadyFree);
        }

        self.data[byte_idx] &= !(1 << bit_idx);
        Ok(())
    }

    /// Marks a contiguous block range as allocated.
    pub fn allocate_range(&mut self, start_idx: u32, count: u32) -> Result<(), BitmapError> {
        for i in 0..count {
            if self.is_allocated(start_idx + i) == Some(true) {
                return Err(BitmapError::AlreadyAllocated);
            }
        }

        for i in 0..count {
            self.allocate(start_idx + i)?;
        }

        Ok(())
    }

    /// Marks a contiguous block range as free.
    pub fn free_range(&mut self, start_idx: u32, count: u32) -> Result<(), BitmapError> {
        for i in 0..count {
            self.free(start_idx + i)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_block_bitmap_basic() {
        let mut data = vec![0u8; 128];
        data[0] = 0b10101010;

        let bitmap = BlockBitmap::new(&mut data, 1024);

        assert_eq!(bitmap.is_allocated(0), Some(false));
        assert_eq!(bitmap.is_allocated(1), Some(true));
        assert_eq!(bitmap.is_allocated(2), Some(false));
        assert_eq!(bitmap.is_allocated(3), Some(true));
    }

    #[test]
    fn test_block_bitmap_find_free() {
        let mut data = vec![0xFFu8; 128];
        data[10] = 0b11111101;

        let bitmap = BlockBitmap::new(&mut data, 1024);

        assert_eq!(bitmap.find_first_free(), Some(10 * 8 + 1));
    }

    #[test]
    fn test_block_bitmap_mut_allocate() {
        let mut data = vec![0u8; 128];
        let mut bitmap = BlockBitmap::new(&mut data, 1024);

        assert!(bitmap.allocate(5).is_ok());
        assert_eq!(bitmap.is_allocated(5), Some(true));
        assert_eq!(bitmap.allocate(5), Err(BitmapError::AlreadyAllocated));
    }
}
