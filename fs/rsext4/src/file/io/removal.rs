//! Validated extent-removal plans and bounded journal restarts.

use super::{
    allocation::extent_allocation_groups,
    transaction::{MetadataTransactionStart, MetadataTransactionStep},
    *,
};

#[derive(Clone, Copy)]
struct ExtentRemovalSegment {
    logical_start: u32,
    physical_start: AbsoluteBN,
    len: u16,
}

pub(super) struct ExtentRemovalPlan {
    segments: Vec<ExtentRemovalSegment>,
    credits: TransactionCredits,
}

/// Validates and records every initialized or unwritten mapping in a range.
///
/// The durable tree is read completely before any data or metadata mutation.
/// The returned credit budget covers every existing extent node, every block
/// allocation group that may change, the inode-table block, and superblock.
pub(super) fn prepare_extent_mapping_removal<B: BlockIo>(
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

pub(super) fn extent_removal_restart_limit<B: BlockIo>(
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

pub(super) fn remove_extent_mapping_with_restarts<B: BlockIo>(
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

pub(super) fn commit_extent_mapping_removal<B: BlockIo>(
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
