//! Immutable index of blocks reserved for ext4 filesystem metadata.

use alloc::{sync::Arc, vec::Vec};

use crate::{
    bitmap::bitmap_utils::set_bit,
    blockgroup_description::Ext4GroupDesc,
    bmalloc::{AbsoluteBN, InodeNumber},
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SystemZone {
    start: u64,
    end: u64,
    owner: Option<InodeNumber>,
}

impl SystemZone {
    fn new(start: u64, count: u64, owner: Option<InodeNumber>) -> Ext4Result<Self> {
        let end = start
            .checked_add(count)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("system_zone:overflow"))?;
        if count == 0 {
            return Err(Ext4Error::corrupted().with_operation("system_zone:empty"));
        }
        Ok(Self { start, end, owner })
    }
}

/// Shareable, read-only metadata-zone index built during mount.
#[derive(Clone, Debug, Default)]
pub(crate) struct SystemZoneMap {
    zones: Arc<[SystemZone]>,
}

impl SystemZoneMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    pub(crate) fn from_layout(
        superblock: &Ext4Superblock,
        group_descs: &[Ext4GroupDesc],
    ) -> Ext4Result<Self> {
        let group_count = superblock.checked_block_groups_count()?;
        if group_descs.len() != group_count as usize {
            return Err(Ext4Error::corrupted().with_operation("system_zone:group_count"));
        }

        let inode_table_blocks = inode_table_blocks(superblock)?;
        let mut zones = Vec::with_capacity(group_descs.len().saturating_mul(4));
        for (group, descriptor) in group_descs.iter().enumerate() {
            let group = u32::try_from(group)
                .map_err(|_| Ext4Error::corrupted().with_operation("system_zone:group_index"))?;
            let base_metadata = base_metadata_blocks(superblock, group, group_count)?;
            if base_metadata != 0 {
                let start = group_first_block(superblock, group)?;
                zones.push(SystemZone::new(start, base_metadata, None)?);
            }

            zones.push(SystemZone::new(descriptor.block_bitmap(), 1, None)?);
            zones.push(SystemZone::new(descriptor.inode_bitmap(), 1, None)?);
            zones.push(SystemZone::new(
                descriptor.inode_table(),
                inode_table_blocks,
                None,
            )?);
        }

        Self::finish(superblock.blocks_count(), zones)
    }

    pub(crate) fn with_owned_blocks(
        &self,
        superblock: &Ext4Superblock,
        owner: InodeNumber,
        blocks: &[AbsoluteBN],
    ) -> Ext4Result<Self> {
        let mut zones = self.zones.to_vec();
        zones.reserve(blocks.len());
        for block in blocks {
            zones.push(SystemZone::new(block.raw(), 1, Some(owner))?);
        }
        Self::finish(superblock.blocks_count(), zones)
    }

    pub(crate) fn allows_range(&self, start: u64, count: u64, inode: InodeNumber) -> bool {
        let Some(end) = start.checked_add(count) else {
            return false;
        };
        let mut index = self.zones.partition_point(|zone| zone.end <= start);
        while let Some(zone) = self.zones.get(index) {
            if zone.start >= end {
                break;
            }
            if zone.owner != Some(inode) {
                return false;
            }
            index += 1;
        }
        true
    }

    /// Synthesizes Linux's in-memory image for an uninitialized block bitmap.
    ///
    /// All filesystem-owned metadata in this group is marked allocated, and
    /// bits beyond the final partial group are marked unavailable. The stale
    /// on-disk bitmap is intentionally ignored until this image is published
    /// together with a descriptor that clears `EXT4_BG_BLOCK_UNINIT`.
    pub(crate) fn initialize_group_block_bitmap(
        &self,
        superblock: &Ext4Superblock,
        group: u32,
        bitmap: &mut [u8],
    ) -> Ext4Result<()> {
        bitmap.fill(0);
        let group_start = group_first_block(superblock, group)?;
        let group_end = group_start
            .checked_add(u64::from(superblock.s_blocks_per_group))
            .ok_or_else(Ext4Error::overflow)?
            .min(superblock.blocks_count());
        let valid_blocks = group_end
            .checked_sub(group_start)
            .ok_or_else(Ext4Error::overflow)?;

        for zone in self.zones.iter().copied() {
            let start = zone.start.max(group_start);
            let end = zone.end.min(group_end);
            for block in start..end {
                let relative =
                    u32::try_from(block - group_start).map_err(|_| Ext4Error::overflow())?;
                if !set_bit(bitmap, relative) {
                    return Err(
                        Ext4Error::corrupted().with_operation("block_bitmap:uninit_metadata")
                    );
                }
            }
        }
        for relative in valid_blocks..u64::from(superblock.s_blocks_per_group) {
            let relative = u32::try_from(relative).map_err(|_| Ext4Error::overflow())?;
            if !set_bit(bitmap, relative) {
                return Err(Ext4Error::corrupted().with_operation("block_bitmap:uninit_tail"));
            }
        }
        Ok(())
    }

    fn finish(filesystem_blocks: u64, mut zones: Vec<SystemZone>) -> Ext4Result<Self> {
        zones.sort_unstable_by_key(|zone| zone.start);
        let mut merged: Vec<SystemZone> = Vec::with_capacity(zones.len());
        for zone in zones {
            if zone.end > filesystem_blocks {
                return Err(Ext4Error::corrupted().with_operation("system_zone:physical_range"));
            }
            if let Some(previous) = merged.last_mut() {
                if zone.start < previous.end {
                    return Err(Ext4Error::corrupted().with_operation("system_zone:overlap"));
                }
                if zone.start == previous.end && zone.owner == previous.owner {
                    previous.end = zone.end;
                    continue;
                }
            }
            merged.push(zone);
        }
        Ok(Self {
            zones: Arc::from(merged.into_boxed_slice()),
        })
    }
}

fn inode_table_blocks(superblock: &Ext4Superblock) -> Ext4Result<u64> {
    let inode_size = u64::from(if superblock.s_inode_size == 0 {
        crate::config::GOOD_OLD_INODE_SIZE
    } else {
        superblock.s_inode_size
    });
    let bytes = u64::from(superblock.s_inodes_per_group)
        .checked_mul(inode_size)
        .ok_or_else(Ext4Error::overflow)?;
    Ok(bytes.div_ceil(u64::from(superblock.checked_block_size()?)))
}

fn group_first_block(superblock: &Ext4Superblock, group: u32) -> Ext4Result<u64> {
    u64::from(group)
        .checked_mul(u64::from(superblock.s_blocks_per_group))
        .and_then(|offset| offset.checked_add(u64::from(superblock.s_first_data_block)))
        .ok_or_else(Ext4Error::overflow)
}

fn base_metadata_blocks(
    superblock: &Ext4Superblock,
    group: u32,
    group_count: u32,
) -> Ext4Result<u64> {
    let has_super = group_has_superblock(superblock, group);
    let mut blocks = u64::from(has_super);
    let descriptors_per_block = superblock.descs_per_block();
    if descriptors_per_block == 0 {
        return Err(Ext4Error::bad_superblock().with_operation("system_zone:descs_per_block"));
    }

    let meta_bg = superblock.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_META_BG);
    let legacy_meta_groups = superblock
        .s_first_meta_bg
        .checked_mul(descriptors_per_block)
        .ok_or_else(Ext4Error::overflow)?;
    if !meta_bg || group < legacy_meta_groups {
        if has_super {
            let gdt_blocks = if meta_bg {
                superblock.s_first_meta_bg
            } else {
                group_count.div_ceil(descriptors_per_block)
            };
            blocks = blocks
                .checked_add(u64::from(gdt_blocks))
                .and_then(|count| count.checked_add(u64::from(superblock.s_reserved_gdt_blocks)))
                .ok_or_else(Ext4Error::overflow)?;
        }
    } else {
        let position = group % descriptors_per_block;
        if position == 0 || position == 1 || position == descriptors_per_block - 1 {
            blocks = blocks.checked_add(1).ok_or_else(Ext4Error::overflow)?;
        }
    }
    Ok(blocks)
}

fn group_has_superblock(superblock: &Ext4Superblock, group: u32) -> bool {
    if group == 0 {
        return true;
    }
    if superblock.has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_SPARSE_SUPER2) {
        return superblock.s_backup_bgs.contains(&group);
    }
    if group == 1 {
        return true;
    }
    if !superblock.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER) {
        return true;
    }
    group % 2 == 1 && (is_power_of(group, 3) || is_power_of(group, 5) || is_power_of(group, 7))
}

fn is_power_of(mut value: u32, base: u32) -> bool {
    while value > 1 && value.is_multiple_of(base) {
        value /= base;
    }
    value == 1
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn owned_zone_allows_only_its_inode() {
        let owner = InodeNumber::new(8).unwrap();
        let other = InodeNumber::new(12).unwrap();
        let map =
            SystemZoneMap::finish(100, vec![SystemZone::new(40, 4, Some(owner)).unwrap()]).unwrap();

        assert!(map.allows_range(40, 4, owner));
        assert!(!map.allows_range(40, 1, other));
        assert!(map.allows_range(44, 1, other));
    }

    #[test]
    fn sparse_superblock_groups_follow_ext4_rules() {
        let sparse = Ext4Superblock {
            s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER,
            ..Default::default()
        };
        for group in [0, 1, 3, 5, 7, 9, 25, 49] {
            assert!(group_has_superblock(&sparse, group));
        }
        for group in [2, 4, 6, 11, 15, 21] {
            assert!(!group_has_superblock(&sparse, group));
        }
    }
}
