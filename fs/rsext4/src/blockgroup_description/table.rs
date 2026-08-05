//! Block group descriptor table views.

use super::desc::Ext4GroupDesc;
use crate::bmalloc::BGIndex;

/// Immutable view over an on-disk block group descriptor table.
#[derive(Debug)]
pub struct BlockGroupDescTable<'a> {
    data: &'a [u8],
    desc_size: usize,
    group_count: u32,
}

impl<'a> BlockGroupDescTable<'a> {
    /// Creates a descriptor table view.
    pub fn new(data: &'a [u8], desc_size: usize, group_count: u32) -> Self {
        Self {
            data,
            desc_size,
            group_count,
        }
    }

    /// Returns the descriptor for the specified group.
    pub fn get_desc(&self, group_idx: BGIndex) -> Option<&Ext4GroupDesc> {
        if group_idx.raw() >= self.group_count {
            return None;
        }

        let offset = group_idx.as_usize().ok()?.checked_mul(self.desc_size)?;
        if offset + core::mem::size_of::<Ext4GroupDesc>() > self.data.len() {
            return None;
        }

        let desc_ptr = self.data[offset..].as_ptr() as *const Ext4GroupDesc;
        unsafe { Some(&*desc_ptr) }
    }

    /// Returns the number of descriptors in the table.
    pub fn group_count(&self) -> u32 {
        self.group_count
    }

    /// Returns the descriptor size in bytes.
    pub fn desc_size(&self) -> usize {
        self.desc_size
    }

    /// Iterates over all descriptors.
    pub fn iter(&'a self) -> BlockGroupDescIter<'a> {
        BlockGroupDescIter {
            table: self,
            current: 0,
        }
    }

    /// Returns the total free block count across all groups.
    pub fn total_free_blocks(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.free_blocks_count() as u64;
        }
        total
    }

    /// Returns the total free inode count across all groups.
    pub fn total_free_inodes(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.free_inodes_count() as u64;
        }
        total
    }

    /// Returns the total used directory count across all groups.
    pub fn total_used_dirs(&self) -> u64 {
        let mut total = 0u64;
        for desc in self.iter() {
            total += desc.used_dirs_count() as u64;
        }
        total
    }

    /// Finds a group with at least `needed` free blocks.
    pub fn find_group_with_free_blocks(&self, needed: u32) -> Option<BGIndex> {
        for raw_idx in 0..self.group_count {
            let idx = BGIndex::new(raw_idx);
            let desc = self.get_desc(idx)?;
            if desc.free_blocks_count() >= needed && !desc.is_block_bitmap_uninit() {
                return Some(idx);
            }
        }
        None
    }

    /// Finds a group that still has free inodes.
    pub fn find_group_with_free_inodes(&self) -> Option<BGIndex> {
        for raw_idx in 0..self.group_count {
            let idx = BGIndex::new(raw_idx);
            let desc = self.get_desc(idx)?;
            if desc.free_inodes_count() > 0 && !desc.is_inode_bitmap_uninit() {
                return Some(idx);
            }
        }
        None
    }
}

/// Iterator over block group descriptors.
pub struct BlockGroupDescIter<'a> {
    table: &'a BlockGroupDescTable<'a>,
    current: u32,
}

impl<'a> Iterator for BlockGroupDescIter<'a> {
    type Item = &'a Ext4GroupDesc;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.table.group_count {
            return None;
        }

        let desc = self.table.get_desc(BGIndex::new(self.current))?;
        self.current += 1;
        Some(desc)
    }
}

/// Mutable view over an on-disk block group descriptor table.
pub struct BlockGroupDescTableMut<'a> {
    data: &'a mut [u8],
    desc_size: usize,
    group_count: u32,
}

impl<'a> BlockGroupDescTableMut<'a> {
    /// Creates a mutable descriptor table view.
    pub fn new(data: &'a mut [u8], desc_size: usize, group_count: u32) -> Self {
        Self {
            data,
            desc_size,
            group_count,
        }
    }

    /// Returns the mutable descriptor for the specified group.
    pub fn get_desc_mut(&mut self, group_idx: BGIndex) -> Option<&mut Ext4GroupDesc> {
        if group_idx.raw() >= self.group_count {
            return None;
        }

        let offset = group_idx.as_usize().ok()?.checked_mul(self.desc_size)?;
        if offset + core::mem::size_of::<Ext4GroupDesc>() > self.data.len() {
            return None;
        }

        let desc_ptr = self.data[offset..].as_mut_ptr() as *mut Ext4GroupDesc;
        unsafe { Some(&mut *desc_ptr) }
    }

    /// Updates the free block count for one group.
    pub fn update_free_blocks(&mut self, group_idx: BGIndex, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_free_blocks_count_lo = (count & 0xFFFF) as u16;
            desc.bg_free_blocks_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// Updates the free inode count for one group.
    pub fn update_free_inodes(&mut self, group_idx: BGIndex, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_free_inodes_count_lo = (count & 0xFFFF) as u16;
            desc.bg_free_inodes_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// Updates the used directory count for one group.
    pub fn update_used_dirs(&mut self, group_idx: BGIndex, count: u32) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// Increments the used directory count for one group.
    pub fn increment_used_dirs(&mut self, group_idx: BGIndex) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            let count = desc.used_dirs_count() + 1;
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// Decrements the used directory count for one group.
    pub fn decrement_used_dirs(&mut self, group_idx: BGIndex) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            let count = desc.used_dirs_count().saturating_sub(1);
            desc.bg_used_dirs_count_lo = (count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = ((count >> 16) & 0xFFFF) as u16;
            true
        } else {
            false
        }
    }

    /// Sets descriptor flags for one group.
    pub fn set_flags(&mut self, group_idx: BGIndex, flags: u16) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_flags |= flags;
            true
        } else {
            false
        }
    }

    /// Clears descriptor flags for one group.
    pub fn clear_flags(&mut self, group_idx: BGIndex, flags: u16) -> bool {
        if let Some(desc) = self.get_desc_mut(group_idx) {
            desc.bg_flags &= !flags;
            true
        } else {
            false
        }
    }
}
