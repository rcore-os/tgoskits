use log::error;

use super::*;
use crate::error::{Ext4Error, Ext4Result};

/// Result of an inode allocation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeAlloc {
    /// Block group index.
    pub group_idx: BGIndex,
    /// Inode index inside the group, zero-based.
    pub inode_in_group: RelativeInodeIndex,
    /// Absolute inode number, one-based.
    pub global_inode: InodeNumber,
}

/// Allocates and frees inodes inside ext4 inode bitmaps.
pub struct InodeAllocator {
    inodes_per_group: u32,
    first_inode: u32,
}

impl InodeAllocator {
    /// Create an inode allocator from the superblock layout.
    pub fn new(sb: &Ext4Superblock) -> Self {
        Self {
            inodes_per_group: sb.s_inodes_per_group,
            first_inode: sb.s_first_ino,
        }
    }

    /// Allocate one inode from the given block group.
    pub fn alloc_inode_in_group(
        &self,
        bitmap_data: &mut [u8],
        group_idx: BGIndex,
        group_desc: &Ext4GroupDesc,
    ) -> Ext4Result<InodeAlloc> {
        // Refuse the request early when the group has no free inodes left.
        if group_desc.free_inodes_count() == 0 {
            return Err(Ext4Error::no_space());
        }

        let mut bitmap = InodeBitmap::new(bitmap_data, self.inodes_per_group);

        // Find the first free inode slot that is eligible for allocation.
        let inode_in_group = self
            .find_free_inode(&bitmap)?
            .ok_or(Ext4Error::no_space())?;

        // Mark the inode as allocated in the bitmap.
        bitmap
            .allocate(inode_in_group)
            .map_err(super::map_bitmap_error)?;

        // Convert the zero-based index into the one-based global inode number.
        let inode_in_group = RelativeInodeIndex::new(inode_in_group);
        let global_inode = self.inode_to_global(group_idx, inode_in_group)?;

        Ok(InodeAlloc {
            group_idx,
            inode_in_group,
            global_inode,
        })
    }

    /// Free one inode in the given block group bitmap.
    pub fn free_inode(
        &self,
        bitmap_data: &mut [u8],
        inode_in_group: RelativeInodeIndex,
    ) -> Ext4Result<()> {
        let mut bitmap = InodeBitmap::new(bitmap_data, self.inodes_per_group);
        bitmap
            .free(inode_in_group.raw())
            .map_err(super::map_bitmap_error)?;
        Ok(())
    }

    pub fn inode_is_free(
        &self,
        bitmap_data: &mut [u8],
        inode_in_group: RelativeInodeIndex,
    ) -> Ext4Result<bool> {
        let bitmap = InodeBitmap::new(bitmap_data, self.inodes_per_group);
        if let Some(resu) = bitmap.is_allocated(inode_in_group.raw()) {
            return Ok(resu);
        }
        error!("bitmap allocted check failed!");
        Err(Ext4Error::invalid_input())
    }

    /// Find the first free inode in a bitmap.
    fn find_free_inode(&self, bitmap: &InodeBitmap) -> Ext4Result<Option<u32>> {
        let start_idx = if self.first_inode > 0 {
            self.first_inode - 1 // Example: first_ino = 11 starts the scan from index 10.
        } else {
            0
        };

        for inode_idx in start_idx..self.inodes_per_group {
            if bitmap.is_allocated(inode_idx) == Some(false) {
                return Ok(Some(inode_idx));
            }
        }
        Ok(None)
    }

    /// Convert a group-local inode index into an absolute inode number.
    fn inode_to_global(
        &self,
        group_idx: BGIndex,
        inode_in_group: RelativeInodeIndex,
    ) -> Ext4Result<InodeNumber> {
        group_idx.inode_number(inode_in_group, self.inodes_per_group)
    }

    /// Convert an absolute inode number into `(group_idx, inode_in_group)`.
    pub fn global_to_group(
        &self,
        global_inode: InodeNumber,
    ) -> Ext4Result<(BGIndex, RelativeInodeIndex)> {
        global_inode.to_group(self.inodes_per_group)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn test_inode_allocator() {
        let sb = Ext4Superblock {
            s_inodes_per_group: 256,
            s_first_ino: 11,
            ..Default::default()
        };

        let allocator = InodeAllocator::new(&sb);

        let mut bitmap_data = vec![0u8; 32]; // 256 bits
        let gd = Ext4GroupDesc {
            bg_free_inodes_count_lo: 256,
            ..Default::default()
        };

        let result = allocator.alloc_inode_in_group(&mut bitmap_data, BGIndex::new(0), &gd);
        assert!(result.is_ok());

        let alloc = result.unwrap();
        assert_eq!(alloc.group_idx, BGIndex::new(0));
        assert!(alloc.inode_in_group.raw() >= 10); // Reserved inodes must be skipped.
    }

    #[test]
    fn test_inode_global_conversion() {
        let sb = Ext4Superblock {
            s_inodes_per_group: 256,
            s_first_ino: 11,
            ..Default::default()
        };

        let allocator = InodeAllocator::new(&sb);

        // Validate the round-trip conversion between global and group-local indices.
        let (group, inode_in_group) = allocator
            .global_to_group(InodeNumber::new(257).unwrap())
            .unwrap();
        assert_eq!(group, BGIndex::new(1));
        assert_eq!(inode_in_group, RelativeInodeIndex::new(0));

        let global = allocator.inode_to_global(group, inode_in_group).unwrap();
        assert_eq!(global, InodeNumber::new(257).unwrap());
    }
}
