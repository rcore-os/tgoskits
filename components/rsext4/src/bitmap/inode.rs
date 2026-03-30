//! Inode bitmap wrappers.

use log::warn;

use crate::bitmap::BitmapError;

/// Inode bitmap view with allocation helpers.
#[derive(Debug)]
pub struct InodeBitmap<'a> {
    data: &'a mut [u8],
    inodes_per_group: u32,
}

impl<'a> InodeBitmap<'a> {
    /// Creates a new inode bitmap view.
    pub fn new(data: &'a mut [u8], inodes_per_group: u32) -> Self {
        Self {
            data,
            inodes_per_group,
        }
    }

    /// Returns whether the inode is allocated.
    pub fn is_allocated(&self, inode_idx: u32) -> Option<bool> {
        if inode_idx >= self.inodes_per_group {
            return None;
        }

        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return None;
        }

        Some((self.data[byte_idx] & (1 << bit_idx)) != 0)
    }

    /// Returns whether the inode is free.
    pub fn is_free(&self, inode_idx: u32) -> Option<bool> {
        self.is_allocated(inode_idx).map(|allocated| !allocated)
    }

    /// Returns the first free inode index.
    pub fn find_first_free(&self) -> Option<u32> {
        for (byte_idx, &byte) in self.data.iter().enumerate() {
            if byte != 0xFF {
                for bit_idx in 0..8 {
                    if (byte & (1 << bit_idx)) == 0 {
                        let inode_idx = (byte_idx * 8 + bit_idx) as u32;
                        if inode_idx < self.inodes_per_group {
                            return Some(inode_idx);
                        }
                    }
                }
            }
        }
        None
    }

    /// Counts free inodes in the group.
    pub fn count_free(&self) -> u32 {
        let mut count = 0u32;

        for inode_idx in 0..self.inodes_per_group {
            if self.is_free(inode_idx) == Some(true) {
                count += 1;
            }
        }

        count
    }

    /// Counts allocated inodes in the group.
    pub fn count_allocated(&self) -> u32 {
        self.inodes_per_group - self.count_free()
    }

    /// Marks an inode as allocated.
    pub fn allocate(&mut self, inode_idx: u32) -> Result<(), BitmapError> {
        if inode_idx >= self.inodes_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }

        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) != 0 {
            return Err(BitmapError::AlreadyAllocated);
        }

        self.data[byte_idx] |= 1 << bit_idx;
        Ok(())
    }

    /// Marks an inode as free.
    pub fn free(&mut self, inode_idx: u32) -> Result<(), BitmapError> {
        if inode_idx >= self.inodes_per_group {
            return Err(BitmapError::IndexOutOfRange);
        }

        let byte_idx = (inode_idx / 8) as usize;
        let bit_idx = (inode_idx % 8) as u8;

        if byte_idx >= self.data.len() {
            return Err(BitmapError::IndexOutOfRange);
        }

        if (self.data[byte_idx] & (1 << bit_idx)) == 0 {
            warn!("Inode num:{inode_idx} already free!");
            return Err(BitmapError::AlreadyFree);
        }

        self.data[byte_idx] &= !(1 << bit_idx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_inode_bitmap_basic() {
        let mut data = vec![0u8; 32];
        data[0] = 0xFF;

        let bitmap = InodeBitmap::new(&mut data, 256);

        assert_eq!(bitmap.find_first_free(), Some(8));
        assert_eq!(bitmap.count_allocated(), 8);
    }
}
