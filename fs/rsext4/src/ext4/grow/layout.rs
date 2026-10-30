//! Target geometry, reserved GDT capacity and journal-credit preflight.

use super::*;
use crate::ext4::system_zone::{group_first_block, group_has_superblock, inode_table_blocks};

/// A target that can be reached using the existing block size and group layout.
pub(super) struct GrowthLayout {
    /// Total filesystem blocks after dropping any unusable final group.
    pub blocks: u64,
    /// Number of block groups covering `blocks`, including a partial last group.
    pub groups: u32,
    /// Filesystem blocks required by one complete target group-descriptor table.
    pub gdt_blocks: u32,
}

impl Ext4Superblock {
    /// Computes the growth target in filesystem blocks from `target_bytes`.
    /// Drops a new final group only when its local metadata cannot fit, and
    /// never shrinks the current filesystem. This read-only geometry check
    /// validates superblock geometry but does not prove feature support,
    /// journal credits, descriptor contents or device durability; callers that
    /// intend to write must still use the mounted growth preflight.
    pub fn growth_target_blocks(&self, target_bytes: u64) -> Ext4Result<u64> {
        self.validate_geometry()?;
        let blocks = target_bytes / u64::from(self.checked_block_size()?);
        if blocks <= self.blocks_count() {
            return Ok(self.blocks_count());
        }
        let groups = self.checked_block_groups_count()?;
        let mut candidate = *self;
        set_blocks(&mut candidate, blocks);
        let last = candidate
            .checked_block_groups_count()?
            .checked_sub(1)
            .ok_or_else(Ext4Error::overflow)?;
        if last < groups {
            return Ok(blocks);
        }
        let available_gdt = groups
            .div_ceil(self.descs_per_block())
            .checked_add(u32::from(self.s_reserved_gdt_blocks))
            .ok_or_else(Ext4Error::overflow)?;
        let overhead = if group_has_superblock(self, last) {
            u64::from(available_gdt) + 1
        } else {
            0
        };
        let metadata = overhead + 2 + inode_table_blocks(self)?;
        let first = group_first_block(self, last)?;
        if blocks - first < metadata {
            Ok(first)
        } else {
            Ok(blocks)
        }
    }
}

impl GrowthLayout {
    /// Checks every intermediate group before any partition-table growth.
    /// `fs` supplies the mounted source layout; `device` supplies durability and
    /// journal limits. `target_bytes` may exceed the current device view.
    /// Returns unchanged geometry for a non-growing target; otherwise rejects
    /// unsupported features, insufficient GDT/journal space and invalid counts.
    pub(super) fn check<B: BlockIo>(
        fs: &Ext4FileSystem,
        device: &Jbd2Dev<B>,
        target_bytes: u64,
    ) -> Ext4Result<Self> {
        let sb = &fs.superblock;
        let current = sb.blocks_count();
        let blocks = target_bytes / fs.block_size() as u64;
        if blocks <= current {
            return Ok(Self {
                blocks: current,
                groups: fs.group_count,
                gdt_blocks: fs.group_count.div_ceil(sb.descs_per_block()),
            });
        }
        // Features that ordinary mounting can ignore are not necessarily safe
        // to grow: they may own backup metadata or require another resize path.
        let supported_compat = Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL
            | Ext4Superblock::EXT4_FEATURE_COMPAT_EXT_ATTR
            | Ext4Superblock::EXT4_FEATURE_COMPAT_RESIZE_INODE
            | Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX;
        if !sb.has_journal()
            || !device.is_use_journal()
            || sb.s_state & Ext4Superblock::EXT4_ERROR_FS != 0
            || sb.s_feature_compat & !supported_compat != 0
            || sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_META_BG)
            || sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_MMP)
            || !device.growth_durability_supported()
        {
            return Err(Ext4Error::unsupported().with_operation("grow:features_or_durability"));
        }
        if !sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT)
            && blocks > u64::from(u32::MAX)
        {
            return Err(Ext4Error::unsupported().with_operation("grow:32bit_capacity"));
        }
        let blocks = sb.growth_target_blocks(target_bytes)?;
        let mut candidate = *sb;
        set_blocks(&mut candidate, blocks);
        let groups = candidate.checked_block_groups_count()?;
        let old_gdt = fs.group_count.div_ceil(sb.descs_per_block());
        let reserved = u32::from(sb.s_reserved_gdt_blocks);
        // Without META_BG, new descriptors must consume the preallocated GDT
        // reserve. Moving an existing bitmap or inode table is outside this path.
        let available_gdt = old_gdt
            .checked_add(reserved)
            .ok_or_else(Ext4Error::overflow)?;
        let gdt_blocks = groups.div_ceil(sb.descs_per_block());
        if gdt_blocks > available_gdt {
            return Err(Ext4Error::unsupported().with_operation("grow:reserved_gdt_capacity"));
        }
        groups
            .checked_mul(sb.s_inodes_per_group)
            .ok_or_else(Ext4Error::overflow)?;
        if fs.group_count.checked_mul(sb.s_inodes_per_group) != Some(sb.s_inodes_count) {
            return Err(Ext4Error::corrupted().with_operation("grow:inode_geometry"));
        }
        // Existing flex groups may place their tables in neighboring groups;
        // new groups use local tables. Validate every intervening backup group,
        // not only the final partial group, before the partition can be grown.
        let inode_blocks = inode_table_blocks(sb)?;
        for group in fs.group_count..groups {
            let available =
                (blocks - group_first_block(sb, group)?).min(u64::from(sb.s_blocks_per_group));
            let overhead = if group_has_superblock(sb, group) {
                u64::from(available_gdt) + 1
            } else {
                0
            };
            if overhead + 2 + inode_blocks > available {
                return Err(
                    Ext4Error::unsupported().with_operation("grow:intermediate_group_metadata")
                );
            }
        }
        let available = device
            .transaction_credit_limit()?
            .ok_or_else(Ext4Error::unsupported)?;
        // Simulate every batch's geometry with the same end/credit rules used
        // by preparation and publication. Unchanged GDT blocks cost no credits.
        let mut stage = *sb;
        while stage.blocks_count() < blocks {
            stage = step::next_geometry(&stage, blocks, available)?;
        }
        Ok(Self {
            blocks,
            groups,
            gdt_blocks,
        })
    }
}
