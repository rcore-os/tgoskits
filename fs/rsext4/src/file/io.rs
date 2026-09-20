use super::*;
use crate::{
    blockdev::{ReservedJournalHandle, TransactionCredits},
    bmalloc::BGIndex,
    endian::DiskFormat,
};

const MAX_RUN_IO_BYTES: usize = 1024 * 1024;

const LINUX_MAX_EXTENT_DEPTH: usize = 5;
const EXT4_META_TRANSACTION_CREDITS_WITHOUT_QUOTA: usize = 6;

/// Options for Linux-style extent preallocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreallocationOptions {
    /// Preserve the current visible file size while reserving blocks.
    pub keep_size: bool,
}

impl PreallocationOptions {
    pub const EXTEND_SIZE: Self = Self { keep_size: false };
    pub const KEEP_SIZE: Self = Self { keep_size: true };
}

/// Options for converting a byte range to unwritten extents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZeroRangeOptions {
    /// Preserve the current visible file size while zeroing the range.
    pub keep_size: bool,
}

impl ZeroRangeOptions {
    pub const EXTEND_SIZE: Self = Self { keep_size: false };
    pub const KEEP_SIZE: Self = Self { keep_size: true };
}

/// One Linux-compatible allocation or mapping operation on a file range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeOperation {
    Allocate(PreallocationOptions),
    PunchHole,
    Zero(ZeroRangeOptions),
    Collapse,
    Insert,
}

/// Applies a typed byte-range operation to an already resolved inode.
pub fn operate_inode_range<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
    operation: RangeOperation,
) -> Ext4Result<()> {
    match operation {
        RangeOperation::Allocate(options) => {
            preallocate_inode(device, fs, inode_num, offset, len, options)
        }
        RangeOperation::PunchHole => punch_hole_inode(device, fs, inode_num, offset, len),
        RangeOperation::Zero(options) => {
            zero_range_inode(device, fs, inode_num, offset, len, options)
        }
        RangeOperation::Collapse => collapse_range_inode(device, fs, inode_num, offset, len),
        RangeOperation::Insert => insert_range_inode(device, fs, inode_num, offset, len),
    }
}

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

fn extent_allocation_groups(
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

/// Releases complete blocks inside a byte range while preserving file size.
pub fn punch_hole_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
) -> Ext4Result<()> {
    let requested_end = checked_range_end(offset, len, "fallocate:punch_zero_length")?;
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    if !inode.is_file() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:not_regular"));
    }
    if offset >= inode.size() {
        return Ok(());
    }

    let block_bytes = fs.block_size() as u64;
    let rounded_size = inode
        .size()
        .checked_next_multiple_of(block_bytes)
        .ok_or_else(Ext4Error::file_too_large)?;
    let end = if requested_end >= inode.size() {
        rounded_size
    } else {
        requested_end
    };
    let full_start = offset.div_ceil(block_bytes);
    let full_end = end / block_bytes;
    if !inode.uses_extents() {
        return punch_legacy_blocks(device, fs, inode_num, inode, offset, end);
    }
    let removal = if full_start < full_end {
        Some(prepare_extent_mapping_removal(
            device, fs, inode_num, &inode, full_start, full_end,
        )?)
    } else {
        None
    };
    let restart_limit = match &removal {
        Some(plan) => {
            extent_removal_restart_limit(device, fs, inode_num, &inode, full_start, full_end, plan)?
        }
        None => None,
    };
    zero_partial_mapped_blocks(device, fs, inode_num, &mut inode, offset, end)?;
    match (removal, restart_limit) {
        (None, _) => fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        ),
        (Some(plan), restart_limit) => {
            if let Some(credit_limit) = restart_limit {
                remove_extent_mapping_with_restarts(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    full_start,
                    full_end,
                    credit_limit,
                )?;
                finalize_restarted_inode_update(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    Ext4InodeMetadataUpdate::write_access(),
                )
            } else {
                commit_extent_mapping_removal(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    Ext4InodeMetadataUpdate::write_access(),
                    None,
                    MetadataTransactionStep {
                        start: MetadataTransactionStart::Join,
                        payload: plan,
                    },
                )
            }
        }
    }
}

/// Converts complete blocks inside a byte range to unwritten extents.
pub fn zero_range_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
    options: ZeroRangeOptions,
) -> Ext4Result<()> {
    let end = checked_range_end(offset, len, "fallocate:zero_zero_length")?;
    let inode = fs.get_inode_by_num(device, inode_num)?;
    if !inode.is_file() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:not_regular"));
    }
    if !inode.uses_extents() {
        return Err(Ext4Error::unsupported().with_operation("fallocate:zero_legacy_indirect"));
    }

    preallocate_inode(
        device,
        fs,
        inode_num,
        offset,
        len,
        PreallocationOptions {
            keep_size: options.keep_size,
        },
    )?;
    let block_bytes = fs.block_size() as u64;
    let full_start = offset.div_ceil(block_bytes);
    let full_end = end / block_bytes;
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    convert_initialized_range_to_unwritten(
        device, fs, inode_num, &mut inode, full_start, full_end,
    )?;
    zero_partial_mapped_blocks(device, fs, inode_num, &mut inode, offset, end)
}

fn checked_range_end(
    offset: u64,
    len: u64,
    zero_length_operation: &'static str,
) -> Ext4Result<u64> {
    if len == 0 {
        return Err(Ext4Error::invalid_input().with_operation(zero_length_operation));
    }
    offset
        .checked_add(len)
        .ok_or_else(Ext4Error::file_too_large)
}

fn zero_partial_mapped_blocks<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    start: u64,
    end: u64,
) -> Ext4Result<()> {
    if start >= end {
        return Ok(());
    }
    let block_bytes = fs.block_size() as u64;
    let start_lbn = start / block_bytes;
    let end_lbn = (end - 1) / block_bytes;
    if !start.is_multiple_of(block_bytes) {
        let block_end = start_lbn
            .checked_add(1)
            .and_then(|logical| logical.checked_mul(block_bytes))
            .ok_or_else(Ext4Error::file_too_large)?;
        zero_mapped_inode_block_slice(
            device,
            fs,
            inode_num,
            inode,
            start_lbn,
            start % block_bytes,
            core::cmp::min(end, block_end) - start_lbn * block_bytes,
        )?;
    }
    if !end.is_multiple_of(block_bytes)
        && (end_lbn != start_lbn || start.is_multiple_of(block_bytes))
    {
        zero_mapped_inode_block_slice(device, fs, inode_num, inode, end_lbn, 0, end % block_bytes)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn zero_mapped_inode_block_slice<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    logical: u64,
    start: u64,
    end: u64,
) -> Ext4Result<()> {
    if start >= end {
        return Ok(());
    }
    let logical = u32::try_from(logical).map_err(|_| Ext4Error::file_too_large())?;
    let Some(physical) = resolve_inode_block(fs, device, inode_num, inode, logical)? else {
        return Ok(());
    };
    let start = usize::try_from(start).map_err(|_| Ext4Error::overflow())?;
    let end = usize::try_from(end).map_err(|_| Ext4Error::overflow())?;
    fs.datablock_cache
        .modify(device, physical, |block| block[start..end].fill(0))
}

#[derive(Clone, Copy)]
struct ExtentRemovalSegment {
    logical_start: u32,
    physical_start: AbsoluteBN,
    len: u16,
}

struct ExtentRemovalPlan {
    segments: Vec<ExtentRemovalSegment>,
    credits: TransactionCredits,
}

#[derive(Clone, Copy)]
enum MetadataTransactionStart {
    Join,
    Restart,
}

struct MetadataTransactionStep<T> {
    start: MetadataTransactionStart,
    payload: T,
}

/// Validates and records every initialized or unwritten mapping in a range.
///
/// The durable tree is read completely before any data or metadata mutation.
/// The returned credit budget covers every existing extent node, every block
/// allocation group that may change, the inode-table block, and superblock.
fn prepare_extent_mapping_removal<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    full_start: u64,
    full_end: u64,
) -> Ext4Result<ExtentRemovalPlan> {
    if full_start >= full_end {
        return Err(Ext4Error::invalid_input().with_operation("extent:remove_range"));
    }
    let mut inode_copy = *inode;
    let mut tree = ExtentTree::with_filesystem(&mut inode_copy, fs, inode_num);
    let external_blocks = tree.external_node_blocks(device)?;
    let extents = tree.all_extents(device)?;
    let mut segments = Vec::new();
    let mut physical_ranges = Vec::new();

    for extent in extents {
        let extent_start = u64::from(extent.ee_block);
        if extent_start >= full_end {
            break;
        }
        let extent_end = extent_start
            .checked_add(u64::from(extent.len()))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        let mut segment_start = core::cmp::max(full_start, extent_start);
        let removal_end = core::cmp::min(extent_end, full_end);
        while segment_start < removal_end {
            let segment_end = removal_end.min(
                segment_start
                    .checked_add(u64::from(Ext4Extent::EXT_UNINIT_MAX_LEN))
                    .ok_or_else(Ext4Error::file_too_large)?,
            );
            let segment_len =
                u32::try_from(segment_end - segment_start).map_err(|_| Ext4Error::overflow())?;
            let physical_start = AbsoluteBN::new(extent.start_block()).checked_add(
                u32::try_from(segment_start - extent_start).map_err(|_| Ext4Error::overflow())?,
            )?;
            segments.push(ExtentRemovalSegment {
                logical_start: u32::try_from(segment_start)
                    .map_err(|_| Ext4Error::file_too_large())?,
                physical_start,
                len: u16::try_from(segment_len).map_err(|_| Ext4Error::overflow())?,
            });
            physical_ranges.push((physical_start, segment_len));
            segment_start = segment_end;
        }
    }

    let credits = if segments.is_empty() {
        TransactionCredits::metadata(1)
    } else {
        let allocation_groups =
            extent_allocation_groups(fs, &physical_ranges, &external_blocks, &[])?;
        let metadata_credits = external_blocks
            .len()
            .checked_add(
                allocation_groups
                    .len()
                    .checked_mul(2)
                    .ok_or_else(Ext4Error::overflow)?,
            )
            .and_then(|credits| credits.checked_add(2))
            .ok_or_else(Ext4Error::overflow)?;
        TransactionCredits::metadata_with_revokes(metadata_credits, external_blocks.len())
    };
    Ok(ExtentRemovalPlan { segments, credits })
}

struct ExtentRemovalChunk {
    plan: ExtentRemovalPlan,
    next_logical: u64,
}

fn extent_removal_restart_limit<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    full_start: u64,
    full_end: u64,
    plan: &ExtentRemovalPlan,
) -> Ext4Result<Option<usize>> {
    let Some(credit_limit) = device.transaction_credit_limit()? else {
        return Ok(None);
    };
    if device.transaction_credit_cost(plan.credits)? <= credit_limit {
        return Ok(None);
    }

    // Validate that even the first bounded step fits before zeroing a partial
    // block or publishing truncate intent. Later steps cannot require more:
    // each covers one allocation group and extent depth can only stay equal or
    // decrease as removal collapses the tree.
    let _ = prepare_extent_mapping_removal_chunk(
        device,
        fs,
        inode_num,
        inode,
        full_start,
        full_end,
        credit_limit,
    )?;
    Ok(Some(credit_limit))
}

fn prepare_extent_mapping_removal_chunk<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    cursor: u64,
    full_end: u64,
    credit_limit: usize,
) -> Ext4Result<Option<ExtentRemovalChunk>> {
    let mut inode_copy = *inode;
    let mut tree = ExtentTree::with_filesystem(&mut inode_copy, fs, inode_num);
    let depth = usize::from(tree.load_root_from_inode()?.header().eh_depth);
    let extents = tree.all_extents(device)?;

    for extent in extents {
        let extent_start = u64::from(extent.ee_block);
        if extent_start >= full_end {
            break;
        }
        let extent_end = extent_start
            .checked_add(u64::from(extent.len()))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        if extent_end <= cursor {
            continue;
        }

        let segment_start = core::cmp::max(cursor, extent_start);
        let physical_start = AbsoluteBN::new(extent.start_block()).checked_add(
            u32::try_from(segment_start - extent_start).map_err(|_| Ext4Error::overflow())?,
        )?;
        let (_, relative_start) = fs.block_allocator.global_to_group(physical_start)?;
        let group_remaining = fs
            .superblock
            .s_blocks_per_group
            .checked_sub(relative_start.raw())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:block_group"))?;
        let segment_capacity =
            core::cmp::min(u32::from(Ext4Extent::EXT_UNINIT_MAX_LEN), group_remaining);
        let segment_limit = segment_start
            .checked_add(u64::from(segment_capacity))
            .ok_or_else(Ext4Error::file_too_large)?;
        let segment_end = core::cmp::min(extent_end, full_end).min(segment_limit);
        if segment_end <= segment_start {
            return Err(Ext4Error::corrupted().with_operation("extent:restart_without_progress"));
        }
        let segment_len =
            u32::try_from(segment_end - segment_start).map_err(|_| Ext4Error::overflow())?;
        let credits = extent_removal_chunk_credits(fs, depth, physical_start, segment_len)?;
        if device.transaction_credit_cost(credits)? > credit_limit {
            return Err(Ext4Error::no_space().with_operation("extent:restart_credits"));
        }
        return Ok(Some(ExtentRemovalChunk {
            plan: ExtentRemovalPlan {
                segments: alloc::vec![ExtentRemovalSegment {
                    logical_start: u32::try_from(segment_start)
                        .map_err(|_| Ext4Error::file_too_large())?,
                    physical_start,
                    len: u16::try_from(segment_len).map_err(|_| Ext4Error::overflow())?,
                }],
                credits,
            },
            next_logical: segment_end,
        }));
    }
    Ok(None)
}

fn extent_removal_chunk_credits(
    fs: &Ext4FileSystem,
    depth: usize,
    physical_start: AbsoluteBN,
    len: u32,
) -> Ext4Result<TransactionCredits> {
    let data_groups = extent_allocation_groups(fs, &[(physical_start, len)], &[], &[])?.len();
    // One removal step can dirty one extent node per tree level and detach at
    // most one node per level. Every released data or metadata block can dirty
    // one block bitmap and one group descriptor; the inode-table block and
    // superblock consume the final two credits. A detached node is either
    // already one of the touched extent nodes or consumes its reserved depth
    // credit as a revoke, so no additional per-level term is required here.
    let allocation_groups = data_groups
        .checked_add(depth)
        .ok_or_else(Ext4Error::overflow)?;
    let metadata_credits = depth
        .checked_add(
            allocation_groups
                .checked_mul(2)
                .ok_or_else(Ext4Error::overflow)?,
        )
        .and_then(|credits| credits.checked_add(2))
        .ok_or_else(Ext4Error::overflow)?;
    Ok(TransactionCredits::metadata_with_revokes(
        metadata_credits,
        depth,
    ))
}

fn remove_extent_mapping_with_restarts<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    full_start: u64,
    full_end: u64,
    credit_limit: usize,
) -> Ext4Result<()> {
    let mut cursor = full_start;
    let mut transaction_start = MetadataTransactionStart::Join;
    while cursor < full_end {
        let Some(chunk) = prepare_extent_mapping_removal_chunk(
            device,
            fs,
            inode_num,
            inode,
            cursor,
            full_end,
            credit_limit,
        )?
        else {
            break;
        };
        commit_extent_mapping_removal(
            device,
            fs,
            inode_num,
            inode,
            Ext4InodeMetadataUpdate::default(),
            None,
            MetadataTransactionStep {
                start: transaction_start,
                payload: chunk.plan,
            },
        )?;
        transaction_start = MetadataTransactionStart::Restart;
        cursor = chunk.next_logical;
    }
    Ok(())
}

fn finalize_restarted_inode_update<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    metadata_update: Ext4InodeMetadataUpdate,
) -> Ext4Result<()> {
    let original_inode = *inode;
    let updated = fs.with_metadata_transaction(device, 1, |fs, device| {
        let mut updated = original_inode;
        fs.finalize_inode_update(device, inode_num, &mut updated, metadata_update)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        Ok(updated)
    })?;
    *inode = updated;
    Ok(())
}

fn begin_restarted_truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    truncate_size: u64,
) -> Ext4Result<()> {
    let original_inode = *inode;
    let updated = fs.with_metadata_transaction(device, 2, |fs, device| {
        let mut updated = original_inode;
        updated.i_size_lo = truncate_size as u32;
        updated.i_size_high = (truncate_size >> 32) as u32;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut updated,
            Ext4InodeMetadataUpdate::truncate_access(),
        )?;
        fs.add_orphan(device, inode_num)?;
        updated = fs.get_inode_by_num(device, inode_num)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        fs.sync_superblock(device)?;
        Ok(updated)
    })?;
    *inode = updated;
    Ok(())
}

fn finish_orphaned_truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
) -> Ext4Result<()> {
    // Removing a non-head orphan can dirty the predecessor inode-table block,
    // the target inode-table block, and the superblock.
    fs.with_metadata_transaction(device, 3, |fs, device| {
        let predecessor = fs.remove_orphan(device, inode_num)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        if let Some(predecessor) = predecessor {
            fs.inodetable_cache.flush(device, predecessor)?;
        }
        fs.sync_superblock(device)?;
        Ok(())
    })
}

fn commit_extent_mapping_removal<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    metadata_update: Ext4InodeMetadataUpdate,
    new_size: Option<u64>,
    transaction: MetadataTransactionStep<ExtentRemovalPlan>,
) -> Ext4Result<()> {
    let MetadataTransactionStep {
        start: transaction_start,
        payload: plan,
    } = transaction;
    let ExtentRemovalPlan { segments, credits } = plan;
    let original_inode = *inode;
    let counters_before = fs.group_counter_snapshot();
    let operation = |fs: &mut Ext4FileSystem, device: &mut Jbd2Dev<B>| {
        let mut updated = original_inode;
        for segment in &segments {
            ExtentTree::with_filesystem(&mut updated, fs, inode_num).remove_extent(
                fs,
                Ext4Extent::new(segment.logical_start, 0, segment.len),
                device,
            )?;
        }
        if let Some(size) = new_size {
            updated.i_size_lo = size as u32;
            updated.i_size_high = (size >> 32) as u32;
        }
        fs.finalize_inode_update(device, inode_num, &mut updated, metadata_update)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        if !segments.is_empty() {
            fs.flush_changed_group_metadata(device, &counters_before)?;
            fs.sync_superblock(device)?;
        }
        for segment in &segments {
            for offset in 0..u32::from(segment.len) {
                fs.datablock_cache
                    .invalidate(segment.physical_start.checked_add(offset)?);
            }
        }
        Ok(updated)
    };
    let updated = match transaction_start {
        MetadataTransactionStart::Join => fs.with_metadata_transaction(device, credits, operation),
        MetadataTransactionStart::Restart => {
            fs.restart_metadata_transaction(device, credits, operation)
        }
    }?;
    *inode = updated;
    Ok(())
}

struct LegacyMappingTransaction {
    plan: crate::indirect::LegacyTruncatePlan,
    footprint: crate::indirect::LegacyTransactionFootprint,
}

struct LegacyMappingChunk {
    transaction: LegacyMappingTransaction,
    next_end: u64,
}

fn build_legacy_mapping_transaction(
    fs: &Ext4FileSystem,
    plan: crate::indirect::LegacyTruncatePlan,
) -> Ext4Result<LegacyMappingTransaction> {
    let footprint = plan.transaction_footprint(fs)?;
    Ok(LegacyMappingTransaction { plan, footprint })
}

fn legacy_mapping_restart_limit<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    full_start: u64,
    full_end: u64,
    transaction: &LegacyMappingTransaction,
) -> Ext4Result<Option<usize>> {
    let Some(limit) = device.transaction_credit_limit()? else {
        return Ok(None);
    };
    if device.transaction_credit_cost(transaction.footprint.credits)? <= limit {
        return Ok(None);
    }
    let chunk = prepare_legacy_mapping_removal_chunk(
        device, fs, inode_num, inode, full_start, full_end, limit,
    )?;
    if chunk.is_none() {
        let _ = prepare_legacy_metadata_cleanup_chunk(
            device, fs, inode_num, inode, full_start, full_end, limit,
        )?
        .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:restart_empty_plan"))?;
    }
    Ok(Some(limit))
}

fn prepare_legacy_mapping_removal_chunk<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    full_start: u64,
    cursor_end: u64,
    credit_limit: usize,
) -> Ext4Result<Option<LegacyMappingChunk>> {
    let mappings = crate::indirect::resolve_all_legacy_inode_blocks(fs, device, inode_num, inode)?;
    let Some((&last_logical, &last_physical)) = mappings.iter().rev().find(|(logical, _)| {
        full_start <= u64::from(**logical) && u64::from(**logical) < cursor_end
    }) else {
        return Ok(None);
    };
    let (last_group, _) = fs.block_allocator.global_to_group(last_physical)?;
    let mut first_logical = last_logical;
    for (&logical, &physical) in mappings.range(..last_logical).rev() {
        if logical.checked_add(1) != Some(first_logical) || u64::from(logical) < full_start {
            break;
        }
        let (group, _) = fs.block_allocator.global_to_group(physical)?;
        if group != last_group {
            break;
        }
        first_logical = logical;
    }

    // Prefer consuming the whole scanned gap so empty indirect branches leave
    // with the neighboring data run. If that footprint is too large, remove
    // one data mapping without advancing the cursor across the unprocessed gap.
    let mut chunk_end = cursor_end;
    let mut plan = crate::indirect::plan_legacy_inode_range_removal(
        fs,
        device,
        inode_num,
        inode,
        u64::from(first_logical),
        chunk_end,
    )?;
    let mut transaction = build_legacy_mapping_transaction(fs, plan)?;
    let next_end = if device.transaction_credit_cost(transaction.footprint.credits)? > credit_limit
    {
        first_logical = last_logical;
        chunk_end = u64::from(last_logical)
            .checked_add(1)
            .ok_or_else(Ext4Error::file_too_large)?;
        plan = crate::indirect::plan_legacy_inode_range_removal(
            fs,
            device,
            inode_num,
            inode,
            u64::from(first_logical),
            chunk_end,
        )?;
        transaction = build_legacy_mapping_transaction(fs, plan)?;
        cursor_end
    } else {
        u64::from(first_logical)
    };
    if device.transaction_credit_cost(transaction.footprint.credits)? > credit_limit {
        return Err(Ext4Error::no_space().with_operation("indirect:restart_credits"));
    }
    Ok(Some(LegacyMappingChunk {
        transaction,
        next_end,
    }))
}

fn prepare_legacy_metadata_cleanup_chunk<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
    full_start: u64,
    cursor_end: u64,
    credit_limit: usize,
) -> Ext4Result<Option<LegacyMappingChunk>> {
    let mut range_start = full_start;
    let mut range_end = cursor_end;
    while range_start < range_end {
        let plan = crate::indirect::plan_legacy_inode_range_removal(
            fs,
            device,
            inode_num,
            inode,
            range_start,
            range_end,
        )?;
        if !plan.has_removals() {
            return Ok(None);
        }
        let transaction = build_legacy_mapping_transaction(fs, plan)?;
        if device.transaction_credit_cost(transaction.footprint.credits)? <= credit_limit {
            return Ok(Some(LegacyMappingChunk {
                transaction,
                next_end: range_start,
            }));
        }
        if range_end - range_start == 1 {
            return Err(Ext4Error::no_space().with_operation("indirect:restart_credits"));
        }

        let midpoint = range_start + (range_end - range_start) / 2;
        let upper = crate::indirect::plan_legacy_inode_range_removal(
            fs, device, inode_num, inode, midpoint, range_end,
        )?;
        if upper.has_removals() {
            range_start = midpoint;
        } else {
            range_end = midpoint;
        }
    }
    Ok(None)
}

fn remove_legacy_mapping_with_restarts<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    full_start: u64,
    full_end: u64,
    credit_limit: usize,
) -> Ext4Result<()> {
    let mut cursor_end = full_end;
    let mut transaction_start = MetadataTransactionStart::Join;
    loop {
        let chunk = match prepare_legacy_mapping_removal_chunk(
            device,
            fs,
            inode_num,
            inode,
            full_start,
            cursor_end,
            credit_limit,
        )? {
            Some(chunk) => chunk,
            None => {
                let Some(chunk) = prepare_legacy_metadata_cleanup_chunk(
                    device,
                    fs,
                    inode_num,
                    inode,
                    full_start,
                    cursor_end,
                    credit_limit,
                )?
                else {
                    return Ok(());
                };
                chunk
            }
        };
        commit_legacy_mapping_removal(
            device,
            fs,
            inode_num,
            inode,
            Ext4InodeMetadataUpdate::default(),
            None,
            MetadataTransactionStep {
                start: transaction_start,
                payload: chunk.transaction,
            },
        )?;
        transaction_start = MetadataTransactionStart::Restart;
        cursor_end = chunk.next_end;
    }
}

fn commit_legacy_mapping_removal<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    metadata_update: Ext4InodeMetadataUpdate,
    new_size: Option<u64>,
    transaction: MetadataTransactionStep<LegacyMappingTransaction>,
) -> Ext4Result<()> {
    let MetadataTransactionStep {
        start: transaction_start,
        payload: transaction,
    } = transaction;
    let LegacyMappingTransaction { plan, footprint } = transaction;
    let original_inode = *inode;
    let operation = |fs: &mut Ext4FileSystem, device: &mut Jbd2Dev<B>| {
        let mut updated = original_inode;
        let block_size = fs.block_size() as u32;
        let huge_file_feature = fs
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
        plan.apply_inode_mapping(&mut updated, block_size, huge_file_feature)?;
        if let Some(size) = new_size {
            updated.i_size_lo = size as u32;
            updated.i_size_high = (size >> 32) as u32;
        }
        plan.apply_pointer_edits(device)?;
        fs.finalize_inode_update(device, inode_num, &mut updated, metadata_update)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        plan.free_removed_blocks(fs, device)?;
        fs.flush_block_allocation_groups(device, &footprint.allocation_groups)?;
        fs.sync_superblock(device)?;
        Ok(updated)
    };
    let updated = match transaction_start {
        MetadataTransactionStart::Join => {
            fs.with_metadata_transaction(device, footprint.credits, operation)
        }
        MetadataTransactionStart::Restart => {
            fs.restart_metadata_transaction(device, footprint.credits, operation)
        }
    }?;
    *inode = updated;
    Ok(())
}

fn punch_legacy_blocks<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    mut inode: Ext4Inode,
    offset: u64,
    end: u64,
) -> Ext4Result<()> {
    let block_bytes = fs.block_size() as u64;
    let full_start = offset.div_ceil(block_bytes);
    let full_end = end / block_bytes;
    let removal = crate::indirect::plan_legacy_inode_range_removal(
        fs, device, inode_num, &inode, full_start, full_end,
    )?;
    let transaction = build_legacy_mapping_transaction(fs, removal)?;
    let restart_limit = legacy_mapping_restart_limit(
        device,
        fs,
        inode_num,
        &inode,
        full_start,
        full_end,
        &transaction,
    )?;
    zero_partial_mapped_blocks(device, fs, inode_num, &mut inode, offset, end)?;
    if let Some(credit_limit) = restart_limit {
        remove_legacy_mapping_with_restarts(
            device,
            fs,
            inode_num,
            &mut inode,
            full_start,
            full_end,
            credit_limit,
        )?;
        finalize_restarted_inode_update(
            device,
            fs,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        )
    } else {
        commit_legacy_mapping_removal(
            device,
            fs,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
            None,
            MetadataTransactionStep {
                start: MetadataTransactionStart::Join,
                payload: transaction,
            },
        )
    }
}

fn convert_initialized_range_to_unwritten<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    full_start: u64,
    full_end: u64,
) -> Ext4Result<()> {
    let mut logical = full_start;
    while logical < full_end {
        let logical_u32 = u32::try_from(logical).map_err(|_| Ext4Error::file_too_large())?;
        let Some(extent) = ExtentTree::with_filesystem(inode, fs, inode_num)
            .find_extent_at_or_after(device, logical_u32)?
        else {
            break;
        };
        let extent_start = u64::from(extent.ee_block);
        if extent_start >= full_end {
            break;
        }
        let extent_end = extent_start
            .checked_add(u64::from(extent.len()))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        let segment_start = core::cmp::max(logical, extent_start);
        let segment_end = core::cmp::min(extent_end, full_end).min(
            segment_start
                .checked_add(u64::from(Ext4Extent::EXT_UNINIT_MAX_LEN))
                .ok_or_else(Ext4Error::file_too_large)?,
        );
        let segment_len =
            u32::try_from(segment_end - segment_start).map_err(|_| Ext4Error::overflow())?;
        if extent.is_initialized() {
            let physical_start = AbsoluteBN::new(extent.start_block()).checked_add(
                u32::try_from(segment_start - extent_start).map_err(|_| Ext4Error::overflow())?,
            )?;
            let depth = ExtentTree::with_filesystem(inode, fs, inode_num)
                .load_root_from_inode()?
                .header()
                .eh_depth;
            let credits = usize::from(depth)
                .checked_mul(2)
                .and_then(|value| value.checked_add(8))
                .ok_or_else(Ext4Error::overflow)?;
            device.with_transaction_handle(credits, |device| {
                ExtentTree::with_filesystem(inode, fs, inode_num).prepare_initialized_zero(
                    fs,
                    device,
                    u32::try_from(segment_start).map_err(|_| Ext4Error::file_too_large())?,
                    segment_len,
                )?;
                fs.modify_inode(device, inode_num, |on_disk| *on_disk = *inode)
            })?;
            for offset in 0..segment_len {
                fs.datablock_cache
                    .invalidate(physical_start.checked_add(offset)?);
            }
        }
        logical = segment_end;
    }
    Ok(())
}

pub fn truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    truncate_size: u64,
) -> Ext4Result<()> {
    let norm_path = normalize_path(path);

    // Resolve the target inode once, then delegate to the inode-based helper.
    let (inode_num, _inode) = match get_inode_with_num(fs, device, &norm_path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };

    truncate_inode(device, fs, inode_num, truncate_size)
}

fn zero_mapped_inode_tail<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    size: u64,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let tail_offset = size % block_bytes;
    if tail_offset == 0 {
        return Ok(());
    }

    let logical = u32::try_from(size / block_bytes).map_err(|_| Ext4Error::file_too_large())?;
    let Some(physical) = resolve_inode_block(fs, device, inode_num, inode, logical)? else {
        return Ok(());
    };
    let tail_offset = usize::try_from(tail_offset).map_err(|_| Ext4Error::overflow())?;
    fs.datablock_cache
        .modify(device, physical, |block| block[tail_offset..].fill(0))
}

fn append_extent_logical_blocks<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    mappings: &alloc::collections::BTreeMap<u32, AbsoluteBN>,
    total_blocks: usize,
    buffer: &mut Vec<u8>,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let total_blocks = u64::try_from(total_blocks).map_err(|_| Ext4Error::file_too_large())?;
    let mapped_blocks = u64::try_from(mappings.len()).map_err(|_| Ext4Error::file_too_large())?;
    let dense = mapped_blocks == total_blocks
        && mappings.first_key_value().map(|(&key, _)| key) == Some(0)
        && mappings
            .last_key_value()
            .is_some_and(|(&key, _)| u64::from(key) + 1 == total_blocks);
    if dense {
        for &physical in mappings.values() {
            let cached = fs.datablock_cache.get_or_load(device, physical)?;
            buffer.extend_from_slice(&cached.data);
        }
        return Ok(());
    }

    let mut next_logical = 0u64;
    for (&logical, &physical) in mappings {
        let logical = u64::from(logical);
        if logical >= total_blocks {
            break;
        }
        while next_logical < logical {
            append_zero_block(buffer, block_size)?;
            next_logical += 1;
        }
        let cached = fs.datablock_cache.get_or_load(device, physical)?;
        buffer.extend_from_slice(&cached.data);
        next_logical = logical + 1;
    }
    while next_logical < total_blocks {
        append_zero_block(buffer, block_size)?;
        next_logical += 1;
    }
    Ok(())
}

pub fn truncate_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
) -> Ext4Result<()> {
    truncate_inode_mapping(
        device,
        fs,
        inode_num,
        truncate_size,
        TruncatePurpose::UserResize,
    )
}

pub(crate) fn recover_linked_truncate_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
) -> Ext4Result<()> {
    truncate_inode_mapping(
        device,
        fs,
        inode_num,
        truncate_size,
        TruncatePurpose::OrphanRecovery,
    )?;
    finish_orphaned_truncate(device, fs, inode_num)
}

pub(crate) fn truncate_inode_for_reap<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
) -> Ext4Result<()> {
    truncate_inode_mapping(device, fs, inode_num, 0, TruncatePurpose::FinalReap)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TruncatePurpose {
    UserResize,
    OrphanRecovery,
    FinalReap,
}

impl TruncatePurpose {
    const fn force_mapping_cleanup(self) -> bool {
        !matches!(self, Self::UserResize)
    }

    const fn accepts_non_file(self) -> bool {
        matches!(self, Self::FinalReap)
    }
}

fn truncate_inode_mapping<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
    purpose: TruncatePurpose,
) -> Ext4Result<()> {
    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    if inode.is_symlink() && !purpose.accepts_non_file() {
        return Err(Ext4Error::unsupported());
    } else if !inode.is_file() && !purpose.accepts_non_file() {
        return Err(Ext4Error::invalid_input());
    }

    let old_size = inode.size();
    if truncate_size == old_size && !purpose.force_mapping_cleanup() {
        return Ok(());
    }

    let block_bytes = fs.block_size() as u64;
    let new_blocks = if truncate_size == 0 {
        0u64
    } else {
        truncate_size.div_ceil(block_bytes)
    };

    // ext4 logical block numbers are u32; reject sizes that need more blocks.
    if new_blocks > u32::MAX as u64 {
        return Err(Ext4Error::file_too_large());
    }

    if truncate_size > old_size {
        if !inode.uses_extents() {
            crate::indirect::validate_legacy_block_count(fs.block_size(), new_blocks)?;
        }
        // Linux clears the old partial EOF before publishing a larger size so
        // bytes hidden by an earlier shrink can never become visible again.
        zero_mapped_inode_tail(device, fs, inode_num, &mut inode, old_size)?;
        inode.i_size_lo = truncate_size as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
        )?;
        return Ok(());
    }

    // Extent-backed files handle extent-aware shrinking here.
    if fs.superblock.has_extents() && inode.uses_extents() {
        if truncate_size < old_size || purpose.force_mapping_cleanup() {
            // Validate and plan the complete initialized/unwritten removal
            // before changing the retained tail or any filesystem metadata.
            let removal = prepare_extent_mapping_removal(
                device,
                fs,
                inode_num,
                &inode,
                new_blocks,
                u64::from(u32::MAX) + 1,
            )?;
            let restart_limit = extent_removal_restart_limit(
                device,
                fs,
                inode_num,
                &inode,
                new_blocks,
                u64::from(u32::MAX) + 1,
                &removal,
            )?;
            zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
            if let Some(credit_limit) = restart_limit {
                if purpose == TruncatePurpose::UserResize {
                    begin_restarted_truncate(device, fs, inode_num, &mut inode, truncate_size)?;
                }
                remove_extent_mapping_with_restarts(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    new_blocks,
                    u64::from(u32::MAX) + 1,
                    credit_limit,
                )?;
                return if purpose == TruncatePurpose::UserResize {
                    finish_orphaned_truncate(device, fs, inode_num)
                } else {
                    inode.i_size_lo = truncate_size as u32;
                    inode.i_size_high = (truncate_size >> 32) as u32;
                    finalize_restarted_inode_update(
                        device,
                        fs,
                        inode_num,
                        &mut inode,
                        Ext4InodeMetadataUpdate::truncate_access(),
                    )
                };
            } else {
                return commit_extent_mapping_removal(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    Ext4InodeMetadataUpdate::truncate_access(),
                    Some(truncate_size),
                    MetadataTransactionStep {
                        start: MetadataTransactionStart::Join,
                        payload: removal,
                    },
                );
            }
        }

        inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        return fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
        );
    }

    let truncate_plan =
        crate::indirect::plan_legacy_inode_truncate(fs, device, inode_num, &inode, new_blocks)?;
    let transaction = build_legacy_mapping_transaction(fs, truncate_plan)?;
    let restart_limit = legacy_mapping_restart_limit(
        device,
        fs,
        inode_num,
        &inode,
        new_blocks,
        u64::from(u32::MAX) + 1,
        &transaction,
    )?;
    // Linux zeros the retained partial EOF block before detaching later
    // mappings. A corrupt hidden branch therefore still fails during the plan
    // preflight before any data or metadata is changed.
    zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
    if let Some(credit_limit) = restart_limit {
        if purpose == TruncatePurpose::UserResize {
            begin_restarted_truncate(device, fs, inode_num, &mut inode, truncate_size)?;
        }
        remove_legacy_mapping_with_restarts(
            device,
            fs,
            inode_num,
            &mut inode,
            new_blocks,
            u64::from(u32::MAX) + 1,
            credit_limit,
        )?;
        if purpose == TruncatePurpose::UserResize {
            finish_orphaned_truncate(device, fs, inode_num)
        } else {
            inode.i_size_lo = truncate_size as u32;
            inode.i_size_high = (truncate_size >> 32) as u32;
            finalize_restarted_inode_update(
                device,
                fs,
                inode_num,
                &mut inode,
                Ext4InodeMetadataUpdate::truncate_access(),
            )
        }
    } else {
        commit_legacy_mapping_removal(
            device,
            fs,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
            Some(truncate_size),
            MetadataTransactionStep {
                start: MetadataTransactionStart::Join,
                payload: transaction,
            },
        )
    }
}

fn read_symlink_target<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<Vec<u8>> {
    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Vec::new());
    }

    // Fast symlinks consume no data blocks. Length alone is insufficient:
    // e2fsprogs stores a 60-byte target in a regular data block.
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    if size <= 60 && inode.blocks_count(fs.block_size() as u32, huge_file_feature) == 0 {
        let mut raw = [0u8; 60];
        for (i, word) in inode.i_block.iter().take(15).enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        return Ok(raw[..size].to_vec());
    }

    let block_bytes = fs.block_size();
    let total_blocks = size.div_ceil(block_bytes);
    let mut buf = Vec::with_capacity(size);

    if inode.uses_extents() {
        let blocks = resolve_inode_blocks(fs, device, inode_num, inode)?;
        append_extent_logical_blocks(device, fs, &blocks, total_blocks, &mut buf)?;
    } else {
        for lbn in 0..total_blocks {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            match resolve_inode_block(fs, device, inode_num, inode, logical)? {
                Some(phys) => {
                    let cached = fs.datablock_cache.get_or_load(device, phys)?;
                    buf.extend_from_slice(&cached.data);
                }
                None => append_zero_block(&mut buf, block_bytes)?,
            }
        }
    }

    buf.truncate(size);

    Ok(buf)
}

fn resolve_symlink_path(current_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return normalize_path(target);
    }
    let parent = match current_path.rfind('/') {
        Some(0) | None => "/",
        Some(pos) => &current_path[..pos],
    };
    let mut combined = String::new();
    if parent == "/" {
        combined.push('/');
        combined.push_str(target);
    } else {
        combined.push_str(parent);
        combined.push('/');
        combined.push_str(target);
    }
    normalize_path(&combined)
}

fn read_file_follow<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    depth: usize,
) -> Ext4Result<Vec<u8>> {
    if depth > 8 {
        return Err(Ext4Error::invalid_input());
    }

    let (inode_num, mut inode) = match get_file_inode(fs, device, path) {
        Ok(Some((ino_num, ino))) => (ino_num, ino),
        Ok(None) => return Err(Ext4Error::not_found()),
        Err(e) => return Err(e),
    };

    if inode.is_symlink() {
        let target_bytes = read_symlink_target(device, fs, inode_num, &mut inode)?;
        let target = match core::str::from_utf8(&target_bytes) {
            Ok(s) => s,
            Err(_) => return Err(Ext4Error::corrupted()),
        };
        let resolved = resolve_symlink_path(path, target);
        return read_file_follow(device, fs, &resolved, depth + 1);
    }

    if !inode.is_file() {
        return Err(if inode.is_dir() {
            Ext4Error::is_dir()
        } else {
            Ext4Error::unsupported()
        });
    }

    let size = inode.size() as usize;
    if size == 0 {
        fs.touch_inode_atime_if_needed(device, inode_num)?;
        return Ok(Vec::new());
    }

    let block_bytes = fs.block_size();
    let total_blocks = size.div_ceil(block_bytes);

    let mut buf = Vec::with_capacity(size);

    if inode.uses_extents() {
        let blocks = resolve_inode_blocks(fs, device, inode_num, &mut inode)?;
        append_extent_logical_blocks(device, fs, &blocks, total_blocks, &mut buf)?;
    } else {
        for lbn in 0..total_blocks {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            match resolve_inode_block(fs, device, inode_num, &mut inode, logical)? {
                Some(phys) => {
                    let cached = fs.datablock_cache.get_or_load(device, phys)?;
                    buf.extend_from_slice(&cached.data);
                }
                None => append_zero_block(&mut buf, block_bytes)?,
            }
        }
    }

    buf.truncate(size);

    fs.touch_inode_atime_if_needed(device, inode_num)?;

    Ok(buf)
}

fn append_zero_block(buffer: &mut Vec<u8>, block_bytes: usize) -> Ext4Result<()> {
    let new_len = buffer
        .len()
        .checked_add(block_bytes)
        .ok_or_else(Ext4Error::overflow)?;
    buffer.resize(new_len, 0);
    Ok(())
}

/// Read the whole file at `path`.
pub fn read_file<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Vec<u8>> {
    read_file_follow(device, fs, path, 0)
}

pub fn read_inode_data_into<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    dst: &mut [u8],
) -> Ext4Result<usize> {
    if dst.is_empty() {
        return Ok(0);
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    let file_size = inode.size();
    if offset >= file_size {
        return Ok(0);
    }

    if inode.is_symlink() {
        let target = read_symlink_target(device, fs, inode_num, &mut inode)?;
        let start = offset as usize;
        let available = target.len().saturating_sub(start);
        let to_read = core::cmp::min(dst.len(), available);
        dst[..to_read].copy_from_slice(&target[start..start + to_read]);
        return Ok(to_read);
    }

    if !inode.is_file() {
        return Err(if inode.is_dir() {
            Ext4Error::is_dir()
        } else {
            Ext4Error::unsupported()
        });
    }

    let to_read = core::cmp::min(dst.len() as u64, file_size - offset) as usize;
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let end = offset + to_read as u64;
    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;

    let mut copied = 0usize;
    if inode.uses_extents() {
        let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
        let runs = tree.initialized_runs_in_range(device, start_lbn as u32, end_lbn as u32)?;
        let mut lbn = start_lbn;
        let max_run_blocks = (MAX_RUN_IO_BYTES / block_size).max(1) as u32;
        for run in runs {
            let run_lbn = u64::from(run.logical_start);
            while lbn < run_lbn {
                let zero_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
                dst[copied..copied + zero_len].fill(0);
                copied += zero_len;
                lbn += 1;
            }

            let mut run_block_offset = 0u32;
            while run_block_offset < run.len {
                let part_blocks = (run.len - run_block_offset).min(max_run_blocks);
                let phys = run.physical_start.checked_add(run_block_offset)?;
                let run_bytes = block_size
                    .checked_mul(part_blocks as usize)
                    .ok_or_else(Ext4Error::overflow)?;
                let mut run_buf = alloc::vec![0; run_bytes];
                fs.datablock_cache
                    .read_run(device, phys, part_blocks, &mut run_buf)?;

                for off in 0..part_blocks {
                    let current_lbn = run_lbn + u64::from(run_block_offset + off);
                    let src_len = copy_len_for_lbn(offset, end, current_lbn, block_bytes)?;
                    let lbn_start = current_lbn * block_bytes;
                    let src_off = (core::cmp::max(offset, lbn_start) - lbn_start) as usize;
                    let run_off = off as usize * block_size + src_off;
                    dst[copied..copied + src_len]
                        .copy_from_slice(&run_buf[run_off..run_off + src_len]);
                    copied += src_len;
                    lbn = current_lbn + 1;
                }
                run_block_offset += part_blocks;
            }
        }
        while lbn <= end_lbn {
            let zero_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
            dst[copied..copied + zero_len].fill(0);
            copied += zero_len;
            lbn += 1;
        }
    } else {
        let mut lbn = start_lbn;
        while lbn <= end_lbn {
            let copy_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
            if let Some(phys) = resolve_inode_block(fs, device, inode_num, &mut inode, lbn as u32)?
            {
                let cached = fs.datablock_cache.get_or_load(device, phys)?;
                let lbn_start = lbn * block_bytes;
                let src_off = (core::cmp::max(offset, lbn_start) - lbn_start) as usize;
                dst[copied..copied + copy_len]
                    .copy_from_slice(&cached.data[src_off..src_off + copy_len]);
            } else {
                dst[copied..copied + copy_len].fill(0);
            }
            copied += copy_len;
            lbn += 1;
        }
    }

    Ok(copied)
}

fn copy_len_for_lbn(offset: u64, end: u64, lbn: u64, block_bytes: u64) -> Ext4Result<usize> {
    let lbn_start = lbn.saturating_mul(block_bytes);
    let lbn_end = lbn_start.saturating_add(block_bytes);
    usize::try_from(core::cmp::min(end, lbn_end) - core::cmp::max(offset, lbn_start))
        .map_err(|_| Ext4Error::overflow())
}

pub fn write_file<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    offset: u64,
    data: &[u8],
) -> Ext4Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    // Resolve the inode once before switching to the inode-based writer.
    let info = match get_inode_with_num(fs, device, path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    let (inode_num, _inode) = info;

    write_inode_data(device, fs, inode_num, offset, data)
}

/// Reserves physical blocks as unwritten extents without exposing old disk data.
pub fn preallocate_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    len: u64,
    options: PreallocationOptions,
) -> Ext4Result<()> {
    if len == 0 {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:zero_length"));
    }
    let end = offset
        .checked_add(len)
        .ok_or_else(Ext4Error::file_too_large)?;
    let block_size = fs.block_size() as u64;
    let start_lbn = offset / block_size;
    let end_lbn = end.div_ceil(block_size);
    if end_lbn > u64::from(u32::MAX) + 1 {
        return Err(Ext4Error::file_too_large());
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    if !inode.is_file() {
        return Err(Ext4Error::invalid_input().with_operation("fallocate:not_regular"));
    }
    if !inode.uses_extents() {
        return Err(Ext4Error::unsupported().with_operation("fallocate:legacy_indirect"));
    }
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let transaction_credits = preallocation_transaction_credits(fs)?;
    let old_size = inode.size();
    let mut logical = start_lbn;
    let mut allocation_error = None;

    while logical < end_lbn {
        let logical_u32 = u32::try_from(logical).map_err(|_| Ext4Error::file_too_large())?;
        let next_extent = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
            .find_extent_at_or_after(device, logical_u32)?;
        if let Some(extent) = next_extent {
            let extent_end = u64::from(extent.ee_block)
                .checked_add(u64::from(extent.len()))
                .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
            if u64::from(extent.ee_block) <= logical && logical < extent_end {
                logical = core::cmp::min(extent_end, end_lbn);
                continue;
            }
        }
        let max_run_end =
            core::cmp::min(end_lbn, logical + u64::from(Ext4Extent::EXT_UNINIT_MAX_LEN));
        let hole_end = next_extent
            .map(|extent| u64::from(extent.ee_block))
            .unwrap_or(end_lbn)
            .min(max_run_end);
        let requested = u32::try_from(hole_end - logical).map_err(|_| Ext4Error::overflow())?;
        let counters_before = fs.group_counter_snapshot();
        let current_inode = inode;
        let chunk = fs.with_metadata_transaction(device, transaction_credits, |fs, device| {
            allocate_unwritten_extent_chunk(
                device,
                fs,
                PreallocationChunk {
                    inode_num,
                    inode: current_inode,
                    logical: logical_u32,
                    requested,
                    huge_file_feature,
                    counters_before: &counters_before,
                },
            )
        });
        match chunk {
            Ok((updated_inode, allocated)) => {
                inode = updated_inode;
                logical += u64::from(allocated);
            }
            Err(error) => {
                allocation_error = Some(error);
                break;
            }
        }
    }

    if !options.keep_size {
        let allocated_end = core::cmp::min(logical.saturating_mul(block_size), end);
        if allocated_end > old_size {
            let current_inode = inode;
            let size_result = fs.with_metadata_transaction(device, 1, |fs, device| {
                let mut updated_inode = current_inode;
                updated_inode.i_size_lo = allocated_end as u32;
                updated_inode.i_size_high = (allocated_end >> 32) as u32;
                fs.finalize_inode_update(
                    device,
                    inode_num,
                    &mut updated_inode,
                    Ext4InodeMetadataUpdate::write_access(),
                )?;
                fs.inodetable_cache.flush(device, inode_num)
            });
            if allocation_error.is_none() {
                size_result?;
            }
        }
    }

    allocation_error.map_or(Ok(()), Err)
}

struct PreallocationChunk<'a> {
    inode_num: InodeNumber,
    inode: Ext4Inode,
    logical: u32,
    requested: u32,
    huge_file_feature: bool,
    counters_before: &'a [GroupCounters],
}

fn preallocation_transaction_credits(fs: &Ext4FileSystem) -> Ext4Result<usize> {
    // Linux ext4_chunk_trans_blocks() reserves for the worst single-extent
    // insertion: two changed blocks per possible tree level plus the new
    // extent, allocation bitmap groups, their descriptor blocks, and the
    // fixed inode/superblock/xattr metadata allowance. Quota is unsupported
    // by this core and therefore contributes no additional credits yet.
    let index_blocks = LINUX_MAX_EXTENT_DEPTH
        .checked_mul(2)
        .and_then(|blocks| blocks.checked_add(1))
        .ok_or_else(Ext4Error::overflow)?;
    let allocation_groups = index_blocks
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?
        .min(usize::try_from(fs.group_count).map_err(|_| Ext4Error::overflow())?);
    let descriptors_per_block =
        usize::try_from(fs.superblock.descs_per_block()).map_err(|_| Ext4Error::overflow())?;
    if descriptors_per_block == 0 {
        return Err(Ext4Error::bad_superblock().with_operation("fallocate:descs_per_block"));
    }
    let descriptor_blocks = usize::try_from(fs.group_count)
        .map_err(|_| Ext4Error::overflow())?
        .div_ceil(descriptors_per_block)
        .min(allocation_groups);

    index_blocks
        .checked_add(allocation_groups)
        .and_then(|credits| credits.checked_add(descriptor_blocks))
        .and_then(|credits| credits.checked_add(EXT4_META_TRANSACTION_CREDITS_WITHOUT_QUOTA))
        .ok_or_else(Ext4Error::overflow)
}

fn allocate_unwritten_extent_chunk<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    chunk: PreallocationChunk<'_>,
) -> Ext4Result<(Ext4Inode, u32)> {
    let PreallocationChunk {
        inode_num,
        mut inode,
        logical,
        requested,
        huge_file_feature,
        counters_before,
    } = chunk;
    let block_size = fs.block_size() as u32;
    let mut accounting_check = inode;
    add_inode_data_blocks(
        &mut accounting_check,
        u64::from(requested),
        block_size,
        huge_file_feature,
    )?;
    let blocks = alloc_contiguous_run_best_effort(device, fs, requested)?;
    let first = *blocks.first().ok_or_else(Ext4Error::no_space)?;
    if blocks
        .windows(2)
        .any(|pair| pair[1].raw() != pair[0].raw() + 1)
    {
        return Err(Ext4Error::corrupted().with_operation("fallocate:noncontiguous_allocator"));
    }
    let allocated = u32::try_from(blocks.len()).map_err(|_| Ext4Error::overflow())?;
    add_inode_data_blocks(
        &mut inode,
        u64::from(allocated),
        block_size,
        huge_file_feature,
    )?;
    let extent = Ext4Extent::new_unwritten(logical, first.raw(), allocated)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("fallocate:extent_length"))?;
    ExtentTree::with_filesystem(&mut inode, fs, inode_num).insert_extent(fs, extent, device)?;
    fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::write_access(),
    )?;
    fs.inodetable_cache.flush(device, inode_num)?;
    fs.flush_changed_group_metadata(device, counters_before)?;
    fs.sync_superblock(device)?;
    Ok((inode, allocated))
}

fn add_inode_data_blocks(
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
        .checked_add(sectors)
        .ok_or_else(Ext4Error::overflow)?;
    inode.set_blocks_count(next, block_size, huge_file_feature)
}

fn subtract_inode_data_blocks(
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

struct WriteSlice<'a> {
    offset: u64,
    end: u64,
    data: &'a [u8],
}

fn write_inode_block_data<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    phys: AbsoluteBN,
    lbn: u64,
    write: &WriteSlice<'_>,
    newly_allocated: bool,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let block_start = lbn.saturating_mul(block_bytes);
    let block_end = block_start.saturating_add(block_bytes);

    let write_start = core::cmp::max(write.offset, block_start);
    let write_end = core::cmp::min(write.end, block_end);
    if write_start >= write_end {
        return Ok(());
    }

    let src_off = usize::try_from(write_start - write.offset).map_err(|_| Ext4Error::overflow())?;
    let dst_off = usize::try_from(write_start - block_start).map_err(|_| Ext4Error::overflow())?;
    let len = usize::try_from(write_end - write_start).map_err(|_| Ext4Error::overflow())?;
    let src_end = src_off.checked_add(len).ok_or_else(Ext4Error::overflow)?;
    let dst_end = dst_off.checked_add(len).ok_or_else(Ext4Error::overflow)?;

    let full_block = dst_off == 0 && len == block_size;
    if newly_allocated || full_block {
        fs.datablock_cache.modify_new(device, phys, |blk| {
            if !full_block {
                blk.fill(0);
            }
            blk[dst_off..dst_end].copy_from_slice(&write.data[src_off..src_end]);
        })?;
    } else {
        fs.datablock_cache.modify(device, phys, |blk| {
            blk[dst_off..dst_end].copy_from_slice(&write.data[src_off..src_end]);
        })?;
    }

    Ok(())
}

fn write_full_block_run<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    start_phys: AbsoluteBN,
    run_start_lbn: u64,
    offset: u64,
    data: &[u8],
    block_count: u32,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let src_off = usize::try_from(run_start_lbn.saturating_mul(block_bytes) - offset)
        .map_err(|_| Ext4Error::overflow())?;
    let byte_len = block_size
        .checked_mul(block_count as usize)
        .ok_or_else(Ext4Error::overflow)?;
    let src_end = src_off
        .checked_add(byte_len)
        .ok_or_else(Ext4Error::overflow)?;
    fs.datablock_cache
        .write_run(device, start_phys, block_count, &data[src_off..src_end])
}

fn existing_full_block_run(
    runs: &[ExtentRun],
    start_lbn: u64,
    offset: u64,
    end: u64,
    block_bytes: u64,
) -> Option<(AbsoluteBN, u32)> {
    let block_start = start_lbn.saturating_mul(block_bytes);
    if offset > block_start {
        return None;
    }

    let run = runs.iter().find(|run| {
        let run_start = u64::from(run.logical_start);
        let run_end = run_start + u64::from(run.len);
        start_lbn >= run_start && start_lbn < run_end
    })?;
    let run_offset = start_lbn.saturating_sub(u64::from(run.logical_start));
    let start_phys = run.physical_start.checked_add(run_offset as u32).ok()?;
    let available_blocks = run.len.saturating_sub(run_offset as u32);
    if available_blocks == 0 {
        return None;
    };
    let max_blocks_by_write = (end - block_start) / block_bytes;
    let run_len = available_blocks.min(max_blocks_by_write as u32);
    if run_len <= 1 {
        return None;
    }
    Some((start_phys, run_len))
}

fn alloc_contiguous_run_best_effort<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    requested: u32,
) -> Ext4Result<Vec<AbsoluteBN>> {
    let mut count = requested.max(1);
    loop {
        match fs.alloc_blocks(device, count) {
            Ok(blocks) => return Ok(blocks),
            Err(err) if err.kind() == Ext4ErrorKind::NoSpace && count > 1 => {
                count = count.div_ceil(2);
            }
            Err(err) => return Err(err),
        }
    }
}

fn rollback_legacy_allocations<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    allocations: Vec<crate::indirect::LegacyBlockAllocation>,
) -> Ext4Result<()> {
    let mut first_error = None;
    for allocation in allocations.into_iter().rev() {
        if let Err(error) = allocation.rollback(fs, device, inode_num, inode)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:legacy_write")),
        None => Ok(()),
    }
}

fn write_legacy_inode_data<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    mut inode: Ext4Inode,
    write: WriteSlice<'_>,
) -> Ext4Result<()> {
    let original_inode = inode;
    let old_size = inode.size();
    let block_bytes = fs.block_size() as u64;
    let start_lbn = write.offset / block_bytes;
    let end_lbn = (write.end - 1) / block_bytes;
    let mut allocations = Vec::new();
    let mut inode_update_attempted = false;
    let operation = (|| {
        for lbn in start_lbn..=end_lbn {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            let allocation = crate::indirect::allocate_legacy_inode_block(
                fs, device, inode_num, &mut inode, logical,
            )?;
            let physical = allocation.physical();
            let newly_allocated = allocation.is_new();
            if newly_allocated {
                allocations.push(allocation);
            }
            write_inode_block_data(device, fs, physical, lbn, &write, newly_allocated)?;
        }

        if write.end > old_size {
            inode.i_size_lo = write.end as u32;
            inode.i_size_high = (write.end >> 32) as u32;
        }
        inode_update_attempted = true;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        )
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(operation_error) => {
            if inode_update_attempted
                && let Err(restore_error) = fs.modify_inode(device, inode_num, |on_disk| {
                    *on_disk = original_inode;
                })
            {
                // The inode cache or pending journal update may still expose
                // the new branch. Retain its blocks unless the old inode image
                // is known to be restored.
                return Err(restore_error.with_operation("rollback:legacy_inode_restore"));
            }
            let cleanup =
                rollback_legacy_allocations(device, fs, inode_num, &mut inode, allocations);
            match cleanup {
                Ok(()) => Err(operation_error),
                Err(cleanup_error) => Err(cleanup_error),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedUnwrittenRun {
    logical_start: u32,
    physical_start: AbsoluteBN,
    len: u32,
}

struct ExtentMetadataSnapshot {
    block: AbsoluteBN,
    bytes: Vec<u8>,
}

fn free_unwritten_finish_reservation<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    reservation: &mut Option<ReservedJournalHandle>,
) -> Ext4Result<()> {
    match reservation.take() {
        Some(reserved) => device.free_reserved_transaction(reserved),
        None => Ok(()),
    }
}

fn finish_prepared_unwritten<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    prepared: &[PreparedUnwrittenRun],
    old_size: u64,
    write_end: u64,
) -> Ext4Result<()> {
    for run in prepared {
        ExtentTree::with_filesystem(inode, fs, inode_num).finish_unwritten_write(
            device,
            run.logical_start,
            run.len,
        )?;
    }
    if write_end > old_size {
        inode.i_size_lo = write_end as u32;
        inode.i_size_high = (write_end >> 32) as u32;
    }
    fs.finalize_inode_update(
        device,
        inode_num,
        inode,
        Ext4InodeMetadataUpdate::write_access(),
    )
}

fn snapshot_prepared_extent_leaves<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    prepared: &[PreparedUnwrittenRun],
) -> Ext4Result<Vec<ExtentMetadataSnapshot>> {
    let mut snapshots = Vec::new();
    for run in prepared {
        let block = ExtentTree::with_filesystem(inode, fs, inode_num)
            .external_leaf_block(device, run.logical_start)?;
        let Some(block) = block else {
            continue;
        };
        if snapshots
            .iter()
            .any(|snapshot: &ExtentMetadataSnapshot| snapshot.block == block)
        {
            continue;
        }
        device.read_block(block)?;
        snapshots.push(ExtentMetadataSnapshot {
            block,
            bytes: device.buffer().to_vec(),
        });
    }
    Ok(snapshots)
}

fn restore_prepared_extent_state<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: Ext4Inode,
    snapshots: &[ExtentMetadataSnapshot],
) -> Ext4Result<()> {
    let mut first_error = None;
    for snapshot in snapshots {
        if let Err(error) = device.write_blocks(&snapshot.bytes, snapshot.block, 1, true)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if let Err(error) = fs.modify_inode(device, inode_num, |on_disk| *on_disk = inode)
        && first_error.is_none()
    {
        first_error = Some(error);
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:unwritten_conversion")),
        None => Ok(()),
    }
}

fn extent_write_needs_preparation<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    start_lbn: u32,
    end_lbn: u32,
) -> Ext4Result<bool> {
    let mut logical = start_lbn;
    loop {
        let next = ExtentTree::with_filesystem(inode, fs, inode_num)
            .find_extent_at_or_after(device, logical)?;
        let Some(extent) = next else {
            return Ok(true);
        };
        if extent.ee_block > logical || extent.is_unwritten() {
            return Ok(true);
        }
        let extent_end = extent
            .ee_block
            .checked_add(extent.len())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        if extent_end > end_lbn {
            return Ok(false);
        }
        logical = extent_end;
    }
}

fn write_inode_data_through_unwritten<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    old_size: u64,
    write: &WriteSlice<'_>,
    start_lbn: u32,
    end_lbn: u32,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let aligned_offset = u64::from(start_lbn)
        .checked_mul(block_bytes)
        .ok_or_else(Ext4Error::file_too_large)?;
    let block_count = u64::from(end_lbn)
        .checked_sub(u64::from(start_lbn))
        .and_then(|blocks| blocks.checked_add(1))
        .ok_or_else(Ext4Error::file_too_large)?;
    let aligned_len = block_count
        .checked_mul(block_bytes)
        .ok_or_else(Ext4Error::file_too_large)?;

    // Fill every hole with an unwritten mapping first. A later data error can
    // therefore leave a reachable reservation, but can never expose stale
    // disk contents as initialized file data.
    preallocate_inode(
        device,
        fs,
        inode_num,
        aligned_offset,
        aligned_len,
        PreallocationOptions::KEEP_SIZE,
    )?;
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    let mut planned = Vec::new();
    let end_exclusive = end_lbn
        .checked_add(1)
        .ok_or_else(Ext4Error::file_too_large)?;
    let mut logical = start_lbn;
    while logical < end_exclusive {
        let extent = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
            .find_extent_at_or_after(device, logical)?
            .ok_or_else(|| Ext4Error::corrupted().with_operation("write:preallocation_hole"))?;
        if extent.ee_block > logical {
            return Err(Ext4Error::corrupted().with_operation("write:preallocation_hole"));
        }
        let extent_end = extent
            .ee_block
            .checked_add(extent.len())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        let run_end = core::cmp::min(extent_end, end_exclusive);
        if extent.is_unwritten() {
            let len = run_end - logical;
            let physical_start =
                AbsoluteBN::new(extent.start_block()).checked_add(logical - extent.ee_block)?;
            planned.push(PreparedUnwrittenRun {
                logical_start: logical,
                physical_start,
                len,
            });
        }
        logical = run_end;
    }

    let reserved_finish_credits = planned
        .len()
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let transaction_credit_limit = device.transaction_credit_limit()?;
    let mut finish_reservation = None;
    let mut prepared = Vec::with_capacity(planned.len());
    for (index, run) in planned.iter().enumerate() {
        let tree_depth = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
            .load_root_from_inode()?
            .header()
            .eh_depth;
        let prepare_credits = usize::from(tree_depth)
            .checked_mul(2)
            .and_then(|credits| credits.checked_add(8))
            .ok_or_else(Ext4Error::overflow)?;
        let is_last = index + 1 == planned.len();
        let reserve_finish = is_last
            && transaction_credit_limit.is_some_and(|limit| {
                reserved_finish_credits <= limit / 2
                    && prepare_credits
                        .checked_add(reserved_finish_credits)
                        .is_some_and(|total| total <= limit)
            });
        let prepare = |device: &mut Jbd2Dev<B>| {
            {
                let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
                tree.prepare_unwritten_write(fs, device, run.logical_start, run.len)?;
            }
            // Publish the still-unwritten split in the same journal operation
            // as its external extent-node updates.
            fs.modify_inode(device, inode_num, |on_disk| *on_disk = inode)
        };
        if reserve_finish {
            let ((), reserved) = device.with_transaction_reservation(
                TransactionCredits::metadata(prepare_credits),
                TransactionCredits::metadata(reserved_finish_credits),
                prepare,
            )?;
            finish_reservation = Some(reserved);
        } else {
            device.with_transaction_handle(prepare_credits, prepare)?;
        }
        prepared.push(*run);
    }

    let leaf_snapshots =
        match snapshot_prepared_extent_leaves(device, fs, inode_num, &mut inode, &prepared) {
            Ok(snapshots) => snapshots,
            Err(error) => {
                let cleanup = free_unwritten_finish_reservation(device, &mut finish_reservation);
                return Err(error_after_cleanup(error, cleanup));
            }
        };

    let data_write = (|| -> Ext4Result<()> {
        let mut lbn = start_lbn;
        while lbn <= end_lbn {
            let physical = if let Some(run) = prepared.iter().find(|run| {
                run.logical_start <= lbn && lbn < run.logical_start.saturating_add(run.len)
            }) {
                let run_offset = lbn - run.logical_start;
                let run_blocks = run
                    .len
                    .checked_sub(run_offset)
                    .ok_or_else(Ext4Error::overflow)?
                    .min(end_lbn - lbn + 1);
                let physical = run.physical_start.checked_add(run_offset)?;
                let run_start = u64::from(lbn)
                    .checked_mul(block_bytes)
                    .ok_or_else(Ext4Error::file_too_large)?;
                let run_end = run_start
                    .checked_add(u64::from(run_blocks) * block_bytes)
                    .ok_or_else(Ext4Error::file_too_large)?;
                if write.offset <= run_start && write.end >= run_end {
                    write_full_block_run(
                        device,
                        fs,
                        physical,
                        u64::from(lbn),
                        write.offset,
                        write.data,
                        run_blocks,
                    )?;
                } else {
                    for offset in 0..run_blocks {
                        write_inode_block_data(
                            device,
                            fs,
                            physical.checked_add(offset)?,
                            u64::from(lbn + offset),
                            write,
                            true,
                        )?;
                    }
                }
                lbn += run_blocks;
                continue;
            } else {
                match ExtentTree::with_filesystem(&mut inode, fs, inode_num)
                    .map_block(device, lbn)?
                {
                    ExtentBlockMapping::Initialized(physical) => physical,
                    ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => {
                        return Err(Ext4Error::corrupted().with_operation("write:prepared_mapping"));
                    }
                }
            };
            write_inode_block_data(device, fs, physical, u64::from(lbn), write, false)?;
            lbn += 1;
        }
        Ok(())
    })();
    if let Err(error) = data_write {
        let cleanup = free_unwritten_finish_reservation(device, &mut finish_reservation);
        return Err(error_after_cleanup(error, cleanup));
    }

    let prepared_inode = inode;
    let Some(finish_credits) = leaf_snapshots.len().checked_add(1) else {
        let cleanup = free_unwritten_finish_reservation(device, &mut finish_reservation);
        return Err(error_after_cleanup(Ext4Error::overflow(), cleanup));
    };
    let finish = match finish_reservation.take() {
        Some(reserved) => device.with_reserved_transaction(reserved, |device| {
            finish_prepared_unwritten(
                device, fs, inode_num, &mut inode, &prepared, old_size, write.end,
            )
        }),
        None => device.with_transaction_handle(finish_credits, |device| {
            finish_prepared_unwritten(
                device, fs, inode_num, &mut inode, &prepared, old_size, write.end,
            )
        }),
    };
    match finish {
        Ok(()) => Ok(()),
        Err(error) => {
            let restore = restore_prepared_extent_state(
                device,
                fs,
                inode_num,
                prepared_inode,
                &leaf_snapshots,
            );
            Err(error_after_cleanup(error, restore))
        }
    }
}

pub fn write_inode_data<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    data: &[u8],
) -> Ext4Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    let old_size = inode.size();
    let block_bytes = fs.block_size() as u64;

    let data_len = u64::try_from(data.len()).map_err(|_| Ext4Error::overflow())?;
    let end = offset
        .checked_add(data_len)
        .ok_or_else(Ext4Error::file_too_large)?;

    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;
    if end_lbn > u32::MAX as u64 {
        return Err(Ext4Error::file_too_large());
    }

    let write = WriteSlice { offset, end, data };
    if !inode.uses_extents() {
        return write_legacy_inode_data(device, fs, inode_num, inode, write);
    }

    if extent_write_needs_preparation(
        device,
        fs,
        inode_num,
        &mut inode,
        start_lbn as u32,
        end_lbn as u32,
    )? {
        return write_inode_data_through_unwritten(
            device,
            fs,
            inode_num,
            old_size,
            &write,
            start_lbn as u32,
            end_lbn as u32,
        );
    }

    let use_existing_run_map = end <= old_size
        && offset.is_multiple_of(block_bytes)
        && end.is_multiple_of(block_bytes)
        && start_lbn < end_lbn;
    let existing_runs = if use_existing_run_map {
        let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
        Some(tree.initialized_runs_in_range(device, start_lbn as u32, end_lbn as u32)?)
    } else {
        None
    };

    let mut lbn = start_lbn;
    while lbn <= end_lbn {
        if let Some(runs) = existing_runs.as_ref()
            && let Some((start_phys, run_len)) =
                existing_full_block_run(runs, lbn, offset, end, block_bytes)
            && run_len > 1
        {
            write_full_block_run(device, fs, start_phys, lbn, offset, data, run_len)?;
            lbn += u64::from(run_len);
            continue;
        }

        let mapping =
            ExtentTree::with_filesystem(&mut inode, fs, inode_num).map_block(device, lbn as u32)?;
        let phys = match mapping {
            ExtentBlockMapping::Initialized(block) => block,
            ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => {
                return Err(Ext4Error::corrupted().with_operation("write:unprepared_mapping"));
            }
        };

        write_inode_block_data(device, fs, phys, lbn, &write, false)?;
        lbn += 1;
    }

    if end > old_size {
        inode.i_size_lo = (end & 0xffff_ffff) as u32;
        inode.i_size_high = (end >> 32) as u32;
    }

    fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::write_access(),
    )?;

    Ok(())
}
