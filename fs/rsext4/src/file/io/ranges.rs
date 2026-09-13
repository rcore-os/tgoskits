//! Typed hole-punching, zeroing, and allocation entry points.

use super::{
    legacy_removal::punch_legacy_blocks,
    removal::{
        commit_extent_mapping_removal, extent_removal_restart_limit,
        prepare_extent_mapping_removal, remove_extent_mapping_with_restarts,
    },
    transaction::{
        MetadataTransactionStart, MetadataTransactionStep, finalize_restarted_inode_update,
    },
    zero::zero_partial_mapped_blocks,
    *,
};

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

pub(super) fn checked_range_end(
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
