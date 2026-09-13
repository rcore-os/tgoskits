//! Allocation-group footprint and inode accounting for released mappings.

use super::*;
use crate::bmalloc::BGIndex;

pub(super) fn extent_allocation_groups(
    fs: &Ext4FileSystem,
    data_ranges: &[(AbsoluteBN, u32)],
    old_external_blocks: &[AbsoluteBN],
    new_external_blocks: &[AbsoluteBN],
) -> Ext4Result<Vec<BGIndex>> {
    let mut groups = Vec::new();
    for block in old_external_blocks
        .iter()
        .chain(new_external_blocks.iter())
        .copied()
    {
        insert_shifted_extent_group(fs, &mut groups, block)?;
    }
    for (start, count) in data_ranges.iter().copied() {
        if count == 0 {
            continue;
        }
        let (first_group, _) = fs.block_allocator.global_to_group(start)?;
        let last = start.checked_add(count - 1)?;
        let (last_group, _) = fs.block_allocator.global_to_group(last)?;
        for raw_group in first_group.raw()..=last_group.raw() {
            insert_group_once(fs, &mut groups, BGIndex::new(raw_group))?;
        }
    }
    Ok(groups)
}

fn insert_shifted_extent_group(
    fs: &Ext4FileSystem,
    groups: &mut Vec<BGIndex>,
    block: AbsoluteBN,
) -> Ext4Result<()> {
    let (group, _) = fs.block_allocator.global_to_group(block)?;
    insert_group_once(fs, groups, group)
}

fn insert_group_once(
    fs: &Ext4FileSystem,
    groups: &mut Vec<BGIndex>,
    group: BGIndex,
) -> Ext4Result<()> {
    if group.raw() >= fs.group_count {
        return Err(Ext4Error::corrupted().with_operation("extent:block_group"));
    }
    if !groups.contains(&group) {
        groups.push(group);
    }
    Ok(())
}

pub(super) fn subtract_inode_data_blocks(
    inode: &mut Ext4Inode,
    blocks: u64,
    block_size: u32,
    huge_file_feature: bool,
) -> Ext4Result<()> {
    let sectors = blocks
        .checked_mul(u64::from(block_size / 512))
        .ok_or_else(Ext4Error::overflow)?;
    let current = inode.blocks_count(block_size, huge_file_feature);
    let next = current
        .checked_sub(sectors)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:block_underflow"))?;
    inode.set_blocks_count(next, block_size, huge_file_feature)
}
