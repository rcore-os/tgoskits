//! Indirect-file mapping removal and its bounded transaction footprint.

use super::{
    transaction::{
        MetadataTransactionStart, MetadataTransactionStep, finalize_restarted_inode_update,
    },
    zero::zero_partial_mapped_blocks,
    *,
};

pub(super) struct LegacyMappingTransaction {
    plan: crate::indirect::LegacyTruncatePlan,
    footprint: crate::indirect::LegacyTransactionFootprint,
}

struct LegacyMappingChunk {
    transaction: LegacyMappingTransaction,
    next_end: u64,
}

pub(super) fn build_legacy_mapping_transaction(
    fs: &Ext4FileSystem,
    plan: crate::indirect::LegacyTruncatePlan,
) -> Ext4Result<LegacyMappingTransaction> {
    let footprint = plan.transaction_footprint(fs)?;
    Ok(LegacyMappingTransaction { plan, footprint })
}

pub(super) fn legacy_mapping_restart_limit<B: BlockIo>(
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

pub(super) fn remove_legacy_mapping_with_restarts<B: BlockIo>(
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

pub(super) fn commit_legacy_mapping_removal<B: BlockIo>(
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

pub(super) fn punch_legacy_blocks<B: BlockIo>(
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
