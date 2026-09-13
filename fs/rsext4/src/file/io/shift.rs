//! Atomic replacement trees for collapse and insert range operations.

use super::{
    allocation::{extent_allocation_groups, subtract_inode_data_blocks},
    ranges::checked_range_end,
    *,
};
use crate::endian::DiskFormat;

/// Removes a byte range and shifts all later extent mappings to the left.
pub fn collapse_range_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
) -> Ext4Result<()> {
    let end = checked_range_end(offset, len, "fallocate:collapse_zero_length")?;
    let block_bytes = fs.block_size() as u64;
    let alignment = fs.superblock.checked_cluster_size()?;
    if !offset.is_multiple_of(alignment) || !len.is_multiple_of(alignment) {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:collapse_alignment"));
    }
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    validate_extent_shift_inode(&inode, "fallocate:collapse_not_extent")?;
    if end >= inode.size() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:collapse_eof"));
    }

    let start_lbn = offset / block_bytes;
    let end_lbn = end / block_bytes;
    let new_size = inode
        .size()
        .checked_sub(len)
        .ok_or_else(Ext4Error::overflow)?;
    rebuild_shifted_extent_mapping(
        device,
        fs,
        inode_num,
        &mut inode,
        ExtentRangeTransform::Collapse {
            start: start_lbn,
            end: end_lbn,
        },
        new_size,
    )
}

/// Inserts a hole and shifts all later extent mappings to the right.
pub fn insert_range_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
) -> Ext4Result<()> {
    checked_range_end(offset, len, "fallocate:insert_zero_length")?;
    let block_bytes = fs.block_size() as u64;
    let alignment = fs.superblock.checked_cluster_size()?;
    if !offset.is_multiple_of(alignment) || !len.is_multiple_of(alignment) {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:insert_alignment"));
    }
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    validate_extent_shift_inode(&inode, "fallocate:insert_not_extent")?;
    if offset >= inode.size() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:insert_eof"));
    }
    let new_size = inode
        .size()
        .checked_add(len)
        .ok_or_else(Ext4Error::file_too_large)?;
    if new_size.div_ceil(block_bytes) > u64::from(u32::MAX) + 1 {
        return Err(Ext4Error::file_too_large());
    }

    rebuild_shifted_extent_mapping(
        device,
        fs,
        inode_num,
        &mut inode,
        ExtentRangeTransform::Insert {
            start: offset / block_bytes,
            len: len / block_bytes,
        },
        new_size,
    )
}

fn validate_extent_shift_inode(
    inode: &Ext4Inode,
    unsupported_operation: &'static str,
) -> Ext4Result<()> {
    if !inode.is_file() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:not_regular"));
    }
    if !inode.uses_extents() {
        return Err(Ext4Error::unsupported().with_operation(unsupported_operation));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ExtentRangeTransform {
    Collapse { start: u64, end: u64 },
    Insert { start: u64, len: u64 },
}

struct TransformedExtents {
    mappings: Vec<Ext4Extent>,
    released_data: Vec<(AbsoluteBN, u32)>,
}

struct ShiftedExtentPlan {
    mappings: Vec<Ext4Extent>,
    released_data: Vec<(AbsoluteBN, u32)>,
    old_external_blocks: Vec<AbsoluteBN>,
}

fn rebuild_shifted_extent_mapping<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    transform: ExtentRangeTransform,
    new_size: u64,
) -> Ext4Result<()> {
    let mut old_tree = ExtentTree::with_filesystem(inode, fs, inode_num);
    let old_external_blocks = old_tree.external_node_blocks(device)?;
    let old_extents = old_tree.all_extents(device)?;
    let TransformedExtents {
        mappings: new_extents,
        released_data: removed_data,
    } = transform_extents(&old_extents, transform)?;

    let plan = ShiftedExtentPlan {
        mappings: new_extents,
        released_data: removed_data,
        old_external_blocks,
    };
    let credits = shifted_extent_transaction_credits(fs, &plan)?;
    let original_inode = *inode;
    let rebuilt = fs.with_metadata_transaction(device, credits, |fs, device| {
        rebuild_shifted_extent_mapping_transaction(
            device,
            fs,
            inode_num,
            original_inode,
            plan,
            new_size,
        )
    })?;
    *inode = rebuilt;
    Ok(())
}

fn rebuild_shifted_extent_mapping_transaction<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    original_inode: Ext4Inode,
    plan: ShiftedExtentPlan,
    new_size: u64,
) -> Ext4Result<Ext4Inode> {
    let ShiftedExtentPlan {
        mappings,
        released_data,
        old_external_blocks,
    } = plan;
    let released_data_blocks = released_data.iter().try_fold(0u64, |total, (_, count)| {
        total
            .checked_add(u64::from(*count))
            .ok_or_else(Ext4Error::overflow)
    })?;
    let released_blocks = released_data_blocks
        .checked_add(u64::try_from(old_external_blocks.len()).map_err(|_| Ext4Error::overflow())?)
        .ok_or_else(Ext4Error::overflow)?;

    // Build the replacement while the old tree remains allocated. The outer
    // filesystem transaction owns every fresh node until the new inode root,
    // allocation bitmaps, descriptors, and superblock are published together.
    let mut rebuilt = original_inode;
    rebuilt.write_extend_header();
    for extent in mappings {
        ExtentTree::with_filesystem(&mut rebuilt, fs, inode_num)
            .insert_extent(fs, extent, device)?;
    }
    rebuilt.i_size_lo = new_size as u32;
    rebuilt.i_size_high = (new_size >> 32) as u32;
    let block_size = fs.block_size() as u32;
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    subtract_inode_data_blocks(&mut rebuilt, released_blocks, block_size, huge_file_feature)?;

    let new_external_blocks =
        ExtentTree::with_filesystem(&mut rebuilt, fs, inode_num).external_node_blocks(device)?;
    let allocation_groups = extent_allocation_groups(
        fs,
        &released_data,
        &old_external_blocks,
        &new_external_blocks,
    )?;

    // Publish the replacement root before returning any old physical block to
    // the allocator. Any later error still rolls the complete transaction back
    // to the original inode, allocator, cache, and device images.
    fs.finalize_inode_update(
        device,
        inode_num,
        &mut rebuilt,
        Ext4InodeMetadataUpdate::write_access(),
    )?;

    for (physical_start, count) in released_data {
        for offset in 0..count {
            let block = physical_start.checked_add(offset)?;
            fs.datablock_cache.invalidate(block);
            fs.free_block(device, block)?;
        }
    }
    for block in old_external_blocks {
        device.forget_detached_metadata(block)?;
        fs.datablock_cache.invalidate(block);
        fs.free_block(device, block)?;
    }

    fs.inodetable_cache.flush(device, inode_num)?;
    fs.flush_block_allocation_groups(device, &allocation_groups)?;
    fs.sync_superblock(device)?;
    Ok(rebuilt)
}

fn shifted_extent_transaction_credits(
    fs: &Ext4FileSystem,
    plan: &ShiftedExtentPlan,
) -> Ext4Result<TransactionCredits> {
    let replacement_nodes = replacement_extent_metadata_blocks(fs, plan.mappings.len())?;
    let released_groups =
        extent_allocation_groups(fs, &plan.released_data, &plan.old_external_blocks, &[])?.len();
    let group_count = usize::try_from(fs.group_count).map_err(|_| Ext4Error::overflow())?;
    let changed_groups = released_groups
        .checked_add(replacement_nodes)
        .ok_or_else(Ext4Error::overflow)?
        .min(group_count);
    let changed_group_credits = changed_groups
        .checked_mul(2)
        .ok_or_else(Ext4Error::overflow)?;

    // Every replacement node has one home block, while every old external
    // node is detached through an independent revoke before allocator reuse.
    // Each potentially affected allocation group contributes at most one
    // bitmap and one primary GDT block; the inode-table block and primary
    // superblock are fixed credits.
    let metadata_credits = replacement_nodes
        .checked_add(changed_group_credits)
        .and_then(|credits| credits.checked_add(2))
        .ok_or_else(Ext4Error::overflow)?;
    Ok(TransactionCredits::metadata_with_revokes(
        metadata_credits,
        plan.old_external_blocks.len(),
    ))
}

fn replacement_extent_metadata_blocks(
    fs: &Ext4FileSystem,
    extent_count: usize,
) -> Ext4Result<usize> {
    const INLINE_ROOT_ENTRIES: usize = 4;
    const FIRST_SPLIT_LEFT_ENTRIES: usize = 2;

    if extent_count <= INLINE_ROOT_ENTRIES {
        return Ok(0);
    }
    let header_size = Ext4ExtentHeader::disk_size();
    let entry_size = core::cmp::max(Ext4Extent::disk_size(), Ext4ExtentIdx::disk_size());
    let node_capacity = fs
        .block_size()
        .checked_sub(header_size)
        .ok_or_else(|| Ext4Error::bad_superblock().with_operation("extent:node_capacity"))?
        / entry_size;
    if node_capacity < INLINE_ROOT_ENTRIES {
        return Err(Ext4Error::bad_superblock().with_operation("extent:node_capacity"));
    }
    let split_occupancy = node_capacity
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?
        / 2;

    let mut depth = 1u16;
    let mut level_nodes =
        external_nodes_for_sorted_entries(extent_count, split_occupancy, FIRST_SPLIT_LEFT_ENTRIES)?;
    let mut total_nodes = level_nodes;
    while level_nodes > INLINE_ROOT_ENTRIES {
        depth = depth.checked_add(1).ok_or_else(Ext4Error::overflow)?;
        if depth > ExtentTree::MAX_DEPTH {
            return Err(Ext4Error::file_too_large().with_operation("extent:depth_overflow"));
        }
        level_nodes = external_nodes_for_sorted_entries(
            level_nodes,
            split_occupancy,
            FIRST_SPLIT_LEFT_ENTRIES,
        )?;
        total_nodes = total_nodes
            .checked_add(level_nodes)
            .ok_or_else(Ext4Error::overflow)?;
    }
    Ok(total_nodes)
}

fn external_nodes_for_sorted_entries(
    entries: usize,
    split_occupancy: usize,
    first_split_left_entries: usize,
) -> Ext4Result<usize> {
    let trailing_entries = entries
        .checked_sub(first_split_left_entries)
        .ok_or_else(Ext4Error::overflow)?;
    trailing_entries
        .div_ceil(split_occupancy)
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)
}

fn transform_extents(
    old_extents: &[Ext4Extent],
    transform: ExtentRangeTransform,
) -> Ext4Result<TransformedExtents> {
    let mut new_extents = Vec::new();
    let mut removed_data = Vec::new();
    for extent in old_extents {
        let extent_start = u64::from(extent.ee_block);
        let extent_end = extent_start
            .checked_add(u64::from(extent.len()))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        match transform {
            ExtentRangeTransform::Collapse { start, end } => {
                let shift = end
                    .checked_sub(start)
                    .ok_or_else(|| Ext4Error::invalid_input().with_operation("extent:collapse"))?;
                if extent_end <= start {
                    push_extent_slice(&mut new_extents, extent, extent_start, 0, extent.len())?;
                } else if extent_start >= end {
                    push_extent_slice(
                        &mut new_extents,
                        extent,
                        extent_start
                            .checked_sub(shift)
                            .ok_or_else(Ext4Error::overflow)?,
                        0,
                        extent.len(),
                    )?;
                } else {
                    if extent_start < start {
                        let left_len = u32::try_from(start - extent_start)
                            .map_err(|_| Ext4Error::overflow())?;
                        push_extent_slice(&mut new_extents, extent, extent_start, 0, left_len)?;
                    }

                    let removed_start = core::cmp::max(extent_start, start);
                    let removed_end = core::cmp::min(extent_end, end);
                    if removed_start < removed_end {
                        let physical_offset = u32::try_from(removed_start - extent_start)
                            .map_err(|_| Ext4Error::overflow())?;
                        let physical_start =
                            AbsoluteBN::new(extent.start_block()).checked_add(physical_offset)?;
                        let count = u32::try_from(removed_end - removed_start)
                            .map_err(|_| Ext4Error::overflow())?;
                        removed_data.push((physical_start, count));
                    }

                    if extent_end > end {
                        let physical_offset =
                            u32::try_from(end - extent_start).map_err(|_| Ext4Error::overflow())?;
                        let right_len =
                            u32::try_from(extent_end - end).map_err(|_| Ext4Error::overflow())?;
                        push_extent_slice(
                            &mut new_extents,
                            extent,
                            end.checked_sub(shift).ok_or_else(Ext4Error::overflow)?,
                            physical_offset,
                            right_len,
                        )?;
                    }
                }
            }
            ExtentRangeTransform::Insert { start, len } => {
                if extent_end <= start {
                    push_extent_slice(&mut new_extents, extent, extent_start, 0, extent.len())?;
                } else if extent_start >= start {
                    push_extent_slice(
                        &mut new_extents,
                        extent,
                        extent_start
                            .checked_add(len)
                            .ok_or_else(Ext4Error::file_too_large)?,
                        0,
                        extent.len(),
                    )?;
                } else {
                    let left_len =
                        u32::try_from(start - extent_start).map_err(|_| Ext4Error::overflow())?;
                    push_extent_slice(&mut new_extents, extent, extent_start, 0, left_len)?;
                    let right_len =
                        u32::try_from(extent_end - start).map_err(|_| Ext4Error::overflow())?;
                    push_extent_slice(
                        &mut new_extents,
                        extent,
                        start
                            .checked_add(len)
                            .ok_or_else(Ext4Error::file_too_large)?,
                        left_len,
                        right_len,
                    )?;
                }
            }
        }
    }
    Ok(TransformedExtents {
        mappings: new_extents,
        released_data: removed_data,
    })
}

fn push_extent_slice(
    output: &mut Vec<Ext4Extent>,
    original: &Ext4Extent,
    logical_start: u64,
    physical_offset: u32,
    len: u32,
) -> Ext4Result<()> {
    if len == 0 {
        return Ok(());
    }
    let logical_start = u32::try_from(logical_start).map_err(|_| Ext4Error::file_too_large())?;
    let physical_start = AbsoluteBN::new(original.start_block()).checked_add(physical_offset)?;
    let mut extent = *original;
    extent.ee_block = logical_start;
    extent.ee_len = original
        .build_len_like(len)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:range_slice"))?;
    extent.ee_start_lo = physical_start.raw() as u32;
    extent.ee_start_hi = (physical_start.raw() >> 32) as u16;
    output.push(extent);
    Ok(())
}
