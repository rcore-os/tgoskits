//! Block group statistics helpers.

use super::desc::Ext4GroupDesc;
use crate::bmalloc::BGIndex;

/// Derived statistics for one block group descriptor.
#[derive(Debug, Clone, Copy)]
pub struct BlockGroupStats {
    pub group_idx: BGIndex,
    pub free_blocks: u32,
    pub free_inodes: u32,
    pub used_dirs: u32,
    pub itable_unused: u32,
    pub flags: u16,
}

impl BlockGroupStats {
    /// Builds a statistics snapshot from one descriptor.
    pub fn from_desc(group_idx: BGIndex, desc: &Ext4GroupDesc) -> Self {
        Self {
            group_idx,
            free_blocks: desc.free_blocks_count(),
            free_inodes: desc.free_inodes_count(),
            used_dirs: desc.used_dirs_count(),
            itable_unused: desc.itable_unused(),
            flags: desc.bg_flags,
        }
    }

    /// Returns the number of used inodes in this group.
    pub fn used_inodes(&self, inodes_per_group: u32) -> u32 {
        inodes_per_group.saturating_sub(self.free_inodes)
    }

    /// Returns the number of used blocks in this group.
    pub fn used_blocks(&self, blocks_per_group: u32) -> u32 {
        blocks_per_group.saturating_sub(self.free_blocks)
    }

    /// Returns block usage as a percentage in the range `[0, 100]`.
    pub fn block_usage_percent(&self, blocks_per_group: u32) -> f32 {
        if blocks_per_group == 0 {
            return 0.0;
        }
        let used = self.used_blocks(blocks_per_group);
        (used as f32 / blocks_per_group as f32) * 100.0
    }

    /// Returns inode usage as a percentage in the range `[0, 100]`.
    pub fn inode_usage_percent(&self, inodes_per_group: u32) -> f32 {
        if inodes_per_group == 0 {
            return 0.0;
        }
        let used = self.used_inodes(inodes_per_group);
        (used as f32 / inodes_per_group as f32) * 100.0
    }
}
