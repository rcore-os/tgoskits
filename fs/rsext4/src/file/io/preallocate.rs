//! Bounded unwritten extent allocation with durable accounting.

use super::*;

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
