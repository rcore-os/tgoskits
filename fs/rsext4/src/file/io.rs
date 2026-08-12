use super::*;

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

    // Build a replacement tree while the old external nodes remain allocated.
    // This guarantees a failed allocation cannot make the durable inode point
    // at a partially built tree, at the cost of temporary metadata headroom.
    let mut rebuilt = *inode;
    rebuilt.write_extend_header();
    for extent in new_extents {
        ExtentTree::with_filesystem(&mut rebuilt, fs, inode_num)
            .insert_extent(fs, extent, device)?;
    }
    rebuilt.i_size_lo = new_size as u32;
    rebuilt.i_size_high = (new_size >> 32) as u32;

    let released_data_blocks = removed_data.iter().try_fold(0u64, |total, (_, count)| {
        total
            .checked_add(u64::from(*count))
            .ok_or_else(Ext4Error::overflow)
    })?;
    let released_blocks = released_data_blocks
        .checked_add(u64::try_from(old_external_blocks.len()).map_err(|_| Ext4Error::overflow())?)
        .ok_or_else(Ext4Error::overflow)?;
    let block_size = fs.block_size() as u32;
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    subtract_inode_data_blocks(&mut rebuilt, released_blocks, block_size, huge_file_feature)?;

    // Publish the replacement root before returning any old physical block to
    // the allocator. Publish final i_blocks accounting with the new root so a
    // later cleanup failure can only leak an unreachable allocation; it cannot
    // expose stale inode accounting or a reference to an already reused block.
    fs.finalize_inode_update(
        device,
        inode_num,
        &mut rebuilt,
        Ext4InodeMetadataUpdate::write_access(),
    )?;

    for (physical_start, count) in removed_data {
        for offset in 0..count {
            let block = physical_start.checked_add(offset)?;
            fs.datablock_cache.invalidate(block);
            fs.free_block(device, block)?;
        }
    }
    for block in old_external_blocks {
        device.forget_detached_metadata(block);
        fs.datablock_cache.invalidate(block);
        fs.free_block(device, block)?;
    }

    *inode = rebuilt;
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
    zero_partial_mapped_blocks(device, fs, inode_num, &mut inode, offset, end)?;
    if full_start >= full_end {
        return fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        );
    }
    punch_extent_blocks(device, fs, inode_num, &mut inode, full_start, full_end)
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

fn punch_extent_blocks<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    full_start: u64,
    full_end: u64,
) -> Ext4Result<()> {
    remove_extent_mapping_range(device, fs, inode_num, inode, full_start, full_end)?;
    fs.finalize_inode_update(
        device,
        inode_num,
        inode,
        Ext4InodeMetadataUpdate::write_access(),
    )
}

/// Removes every allocated extent segment in a logical block range.
///
/// Unlike `resolve_inode_blocks`, this walker deliberately includes unwritten
/// extents. Each bounded edit publishes the updated inode root in the same
/// transaction as the extent-tree edit; operation-specific timestamps and file
/// size are finalized by the caller after the complete range is detached.
fn remove_extent_mapping_range<B: BlockIo>(
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
        let physical_start = AbsoluteBN::new(extent.start_block()).checked_add(
            u32::try_from(segment_start - extent_start).map_err(|_| Ext4Error::overflow())?,
        )?;
        let depth = ExtentTree::with_filesystem(inode, fs, inode_num)
            .load_root_from_inode()?
            .header()
            .eh_depth;
        let credits = usize::from(depth)
            .checked_mul(2)
            .and_then(|value| value.checked_add(12))
            .ok_or_else(Ext4Error::overflow)?;
        device.with_transaction_handle(credits, |device| {
            ExtentTree::with_filesystem(inode, fs, inode_num).remove_extent(
                fs,
                Ext4Extent::new(
                    u32::try_from(segment_start).map_err(|_| Ext4Error::file_too_large())?,
                    0,
                    u16::try_from(segment_len).map_err(|_| Ext4Error::overflow())?,
                ),
                device,
            )?;
            let updated = *inode;
            fs.modify_inode(device, inode_num, |on_disk| *on_disk = updated)
        })?;
        for offset in 0..segment_len {
            fs.datablock_cache
                .invalidate(physical_start.checked_add(offset)?);
        }
        logical = segment_end;
    }
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
    let original_inode = inode;
    let removal = crate::indirect::plan_legacy_inode_range_removal(
        fs, device, inode_num, &inode, full_start, full_end,
    )?;
    zero_partial_mapped_blocks(device, fs, inode_num, &mut inode, offset, end)?;
    let block_size = fs.block_size() as u32;
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    removal.apply_inode_mapping(&mut inode, block_size, huge_file_feature)?;
    removal.apply_pointer_edits(device)?;
    if let Err(operation_error) = fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::write_access(),
    ) {
        let mut rollback_error = removal.restore_pointer_edits(device).err();
        if let Err(error) = fs.modify_inode(device, inode_num, |on_disk| {
            *on_disk = original_inode;
        }) && rollback_error.is_none()
        {
            rollback_error = Some(error);
        }
        return Err(match rollback_error {
            Some(error) => error.with_operation("rollback:indirect_punch"),
            None => operation_error,
        });
    }
    removal.free_removed_blocks(fs, device)
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
    )
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
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
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
            // Clear the retained partial EOF before detaching later mappings,
            // then enumerate the extent tree directly so unwritten allocations
            // are released rather than being mistaken for holes.
            zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
            remove_extent_mapping_range(
                device,
                fs,
                inode_num,
                &mut inode,
                new_blocks,
                u64::from(u32::MAX) + 1,
            )?;
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

    let original_inode = inode;
    let truncate_plan =
        crate::indirect::plan_legacy_inode_truncate(fs, device, inode_num, &inode, new_blocks)?;
    // Linux zeros the retained partial EOF block before detaching later
    // mappings. A corrupt hidden branch therefore still fails during the plan
    // preflight before any data or metadata is changed.
    zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
    truncate_plan.apply_inode_mapping(&mut inode, block_bytes as u32, huge_file_feature)?;
    inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
    inode.i_size_high = (truncate_size >> 32) as u32;
    truncate_plan.apply_pointer_edits(device)?;

    if let Err(operation_error) = fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::truncate_access(),
    ) {
        let mut rollback_error = truncate_plan.restore_pointer_edits(device).err();
        if let Err(error) = fs.modify_inode(device, inode_num, |on_disk| {
            *on_disk = original_inode;
        }) && rollback_error.is_none()
        {
            rollback_error = Some(error);
        }
        return Err(match rollback_error {
            Some(error) => error.with_operation("rollback:indirect_truncate"),
            None => operation_error,
        });
    }

    truncate_plan.free_removed_blocks(fs, device)
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
        fs.touch_inode_atime_if_needed(device, inode_num)?;
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

    fs.touch_inode_atime_if_needed(device, inode_num)?;
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
    let mut prepared = Vec::new();
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
            let tree_depth = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
                .load_root_from_inode()?
                .header()
                .eh_depth;
            let prepare_credits = usize::from(tree_depth)
                .checked_mul(2)
                .and_then(|credits| credits.checked_add(8))
                .ok_or_else(Ext4Error::overflow)?;
            device.with_transaction_handle(prepare_credits, |device| {
                {
                    let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
                    tree.prepare_unwritten_write(fs, device, logical, len)?;
                }
                // Publish the still-unwritten split in the same journal
                // operation as its external extent-node updates.
                fs.modify_inode(device, inode_num, |on_disk| *on_disk = inode)
            })?;
            prepared.push(PreparedUnwrittenRun {
                logical_start: logical,
                physical_start,
                len,
            });
        }
        logical = run_end;
    }

    let leaf_snapshots =
        snapshot_prepared_extent_leaves(device, fs, inode_num, &mut inode, &prepared)?;

    let mut lbn = start_lbn;
    while lbn <= end_lbn {
        let physical = if let Some(run) = prepared
            .iter()
            .find(|run| run.logical_start <= lbn && lbn < run.logical_start.saturating_add(run.len))
        {
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
            match ExtentTree::with_filesystem(&mut inode, fs, inode_num).map_block(device, lbn)? {
                ExtentBlockMapping::Initialized(physical) => physical,
                ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => {
                    return Err(Ext4Error::corrupted().with_operation("write:prepared_mapping"));
                }
            }
        };
        write_inode_block_data(device, fs, physical, u64::from(lbn), write, false)?;
        lbn += 1;
    }

    let prepared_inode = inode;
    let finish_credits = leaf_snapshots
        .len()
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let finish = device.with_transaction_handle(finish_credits, |device| {
        for run in &prepared {
            ExtentTree::with_filesystem(&mut inode, fs, inode_num).finish_unwritten_write(
                device,
                run.logical_start,
                run.len,
            )?;
        }
        if write.end > old_size {
            inode.i_size_lo = write.end as u32;
            inode.i_size_high = (write.end >> 32) as u32;
        }
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        )
    });
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
