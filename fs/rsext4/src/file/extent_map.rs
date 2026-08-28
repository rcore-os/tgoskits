//! Portable file-to-device extent inspection.

use alloc::vec::Vec;

use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    bmalloc::InodeNumber,
    error::{Ext4Error, Ext4Result},
    extents_tree::ExtentTree,
    indirect::resolve_legacy_inode_blocks,
    superblock::Ext4Superblock,
};

const MAX_LFS_FILESIZE: u64 = i64::MAX as u64;
const DIRECT_BLOCKS: u64 = 12;

/// Allocation state of one mapped file extent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileExtentState {
    Initialized,
    Unwritten,
    /// Metadata resides directly in the inode body rather than in a block.
    Inline,
}

/// On-disk mapping namespace selected for an extent query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileExtentTarget {
    Data,
    ExtendedAttributes,
}

/// One byte-addressed file-to-device mapping returned for inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileExtent {
    pub logical_start: u64,
    pub physical_start: u64,
    pub length: u64,
    pub state: FileExtentState,
    /// Legacy indirect mappings are merged from adjacent block pointers.
    pub merged: bool,
}

/// Bounded result of one file extent query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileExtentMap {
    pub extents: Vec<FileExtent>,
    /// Number of mappings reported by this query. In count-only mode this may
    /// exceed `extents.len()` because no extent records are materialized.
    pub mapped_extents: usize,
    /// Whether the result reached the final mapping in the requested range.
    pub complete: bool,
}

/// Inspects allocated inode mappings in a byte range without exposing disk structs.
pub fn inspect_inode_extents<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    start: u64,
    length: u64,
    target: FileExtentTarget,
    extent_limit: usize,
) -> Ext4Result<FileExtentMap> {
    if length == 0 {
        return Err(Ext4Error::invalid_input().with_operation("inode:extent_query_length"));
    }
    let mut inode = filesystem.get_inode_by_num(device, inode_number)?;
    if !inode.is_file() && !inode.is_dir() {
        return Err(Ext4Error::unsupported().with_operation("inode:extent_query_type"));
    }

    let block_size = filesystem.block_size() as u64;
    let maximum_file_bytes = maximum_inode_bytes(filesystem, inode.uses_extents())?;
    if start > maximum_file_bytes {
        return Err(Ext4Error::file_too_large().with_operation("inode:extent_query_start"));
    }
    if target == FileExtentTarget::ExtendedAttributes {
        return super::xattr_extent::inspect_xattr_extent(
            device,
            filesystem,
            inode_number,
            &inode,
            start,
            length,
            extent_limit,
        );
    }
    let end = start
        .saturating_add(length)
        .min(maximum_file_bytes)
        .min(filesystem.inode_size(&inode));
    if start >= end {
        return Ok(FileExtentMap {
            extents: Vec::new(),
            mapped_extents: 0,
            complete: true,
        });
    }

    let mut mappings = Vec::new();
    if inode.uses_extents() {
        let extents = ExtentTree::with_filesystem(&mut inode, filesystem, inode_number)
            .all_extents(device)?;
        for extent in extents {
            let logical_start = u64::from(extent.ee_block)
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            let extent_length = u64::from(extent.len())
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            let physical_start = extent
                .start_block()
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            push_overlapping_extent(
                &mut mappings,
                start,
                end,
                FileExtent {
                    logical_start,
                    physical_start,
                    length: extent_length,
                    state: if extent.is_unwritten() {
                        FileExtentState::Unwritten
                    } else {
                        FileExtentState::Initialized
                    },
                    merged: false,
                },
            )?;
        }
    } else {
        for (logical_block, physical_block) in
            resolve_legacy_inode_blocks(filesystem, device, inode_number, &inode)?
        {
            let logical_start = u64::from(logical_block)
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            let physical_start = physical_block
                .raw()
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            push_overlapping_extent(
                &mut mappings,
                start,
                end,
                FileExtent {
                    logical_start,
                    physical_start,
                    length: block_size,
                    state: FileExtentState::Initialized,
                    merged: true,
                },
            )?;
        }
    }

    finish_extent_map(mappings, extent_limit)
}

pub(super) fn finish_extent_map(
    mut mappings: Vec<FileExtent>,
    extent_limit: usize,
) -> Ext4Result<FileExtentMap> {
    let total = mappings.len();
    if extent_limit == 0 {
        mappings.clear();
    } else if mappings.len() > extent_limit {
        mappings.truncate(extent_limit);
    }
    Ok(FileExtentMap {
        mapped_extents: if extent_limit == 0 {
            total
        } else {
            mappings.len()
        },
        complete: extent_limit == 0 || total <= extent_limit,
        extents: mappings,
    })
}

pub(super) fn maximum_inode_bytes(
    filesystem: &Ext4FileSystem,
    uses_extents: bool,
) -> Ext4Result<u64> {
    let block_size = filesystem.block_size() as u64;
    if block_size < 512 || !block_size.is_power_of_two() {
        return Err(Ext4Error::bad_superblock().with_operation("inode:maximum_file_block_size"));
    }
    let block_bits = block_size.trailing_zeros();
    let sector_shift = block_bits.checked_sub(9).ok_or_else(Ext4Error::overflow)?;
    let has_huge_files = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);

    if uses_extents {
        let extent_limit = u64::from(u32::MAX)
            .checked_shl(block_bits)
            .ok_or_else(Ext4Error::overflow)?;
        let accounting_limit = if has_huge_files {
            MAX_LFS_FILESIZE
        } else {
            u64::from(u32::MAX)
                .checked_shr(sector_shift)
                .and_then(|blocks| blocks.checked_shl(block_bits))
                .ok_or_else(Ext4Error::overflow)?
        };
        return Ok(extent_limit.min(accounting_limit));
    }

    let pointers_per_block = block_size
        .checked_div(core::mem::size_of::<u32>() as u64)
        .filter(|pointers| *pointers != 0)
        .ok_or_else(|| Ext4Error::bad_superblock().with_operation("inode:maximum_file_pointers"))?;
    maximum_legacy_bytes(block_bits, sector_shift, pointers_per_block, has_huge_files)
}

fn maximum_legacy_bytes(
    block_bits: u32,
    sector_shift: u32,
    pointers_per_block: u64,
    has_huge_files: bool,
) -> Ext4Result<u64> {
    let upper_limit = if has_huge_files {
        (1_u64 << 48) - 1
    } else {
        u64::from(u32::MAX)
            .checked_shr(sector_shift)
            .ok_or_else(Ext4Error::overflow)?
    };
    let double = pointers_per_block
        .checked_mul(pointers_per_block)
        .ok_or_else(Ext4Error::overflow)?;
    let triple = double
        .checked_mul(pointers_per_block)
        .ok_or_else(Ext4Error::overflow)?;
    let tree_data_blocks = DIRECT_BLOCKS
        .checked_add(pointers_per_block)
        .and_then(|blocks| blocks.checked_add(double))
        .and_then(|blocks| blocks.checked_add(triple))
        .ok_or_else(Ext4Error::overflow)?;
    let full_tree_metadata = 3_u64
        .checked_add(
            pointers_per_block
                .checked_mul(2)
                .ok_or_else(Ext4Error::overflow)?,
        )
        .and_then(|blocks| blocks.checked_add(double))
        .ok_or_else(Ext4Error::overflow)?;

    let data_blocks = if tree_data_blocks
        .checked_add(full_tree_metadata)
        .ok_or_else(Ext4Error::overflow)?
        <= upper_limit
    {
        tree_data_blocks
    } else {
        let mut remaining = upper_limit
            .checked_sub(DIRECT_BLOCKS)
            .and_then(|blocks| blocks.checked_sub(pointers_per_block))
            .ok_or_else(Ext4Error::overflow)?;
        let mut metadata_blocks = 1_u64;
        if remaining < double {
            metadata_blocks = metadata_blocks
                .checked_add(1)
                .and_then(|blocks| {
                    divide_round_up(remaining, pointers_per_block)
                        .and_then(|indirect| blocks.checked_add(indirect))
                })
                .ok_or_else(Ext4Error::overflow)?;
        } else {
            metadata_blocks = metadata_blocks
                .checked_add(1)
                .and_then(|blocks| blocks.checked_add(pointers_per_block))
                .ok_or_else(Ext4Error::overflow)?;
            remaining = remaining
                .checked_sub(double)
                .ok_or_else(Ext4Error::overflow)?;
            metadata_blocks = metadata_blocks
                .checked_add(1)
                .and_then(|blocks| {
                    divide_round_up(remaining, pointers_per_block)
                        .and_then(|indirect| blocks.checked_add(indirect))
                })
                .and_then(|blocks| {
                    divide_round_up(remaining, double)
                        .and_then(|double_indirect| blocks.checked_add(double_indirect))
                })
                .ok_or_else(Ext4Error::overflow)?;
        }
        upper_limit
            .checked_sub(metadata_blocks)
            .ok_or_else(Ext4Error::overflow)?
    };

    Ok(data_blocks
        .checked_shl(block_bits)
        .ok_or_else(Ext4Error::overflow)?
        .min(MAX_LFS_FILESIZE))
}

fn divide_round_up(value: u64, divisor: u64) -> Option<u64> {
    value
        .checked_add(divisor.checked_sub(1)?)
        .map(|adjusted| adjusted / divisor)
}

pub(super) fn push_overlapping_extent(
    output: &mut Vec<FileExtent>,
    query_start: u64,
    query_end: u64,
    mapping: FileExtent,
) -> Ext4Result<()> {
    let mapping_end = mapping
        .logical_start
        .checked_add(mapping.length)
        .ok_or_else(Ext4Error::overflow)?;
    if mapping.logical_start >= query_end || mapping_end <= query_start {
        return Ok(());
    }

    if let Some(previous) = output.last_mut() {
        let previous_logical_end = previous
            .logical_start
            .checked_add(previous.length)
            .ok_or_else(Ext4Error::overflow)?;
        let previous_physical_end = previous
            .physical_start
            .checked_add(previous.length)
            .ok_or_else(Ext4Error::overflow)?;
        if previous_logical_end == mapping.logical_start
            && previous_physical_end == mapping.physical_start
            && previous.state == mapping.state
            && previous.merged == mapping.merged
        {
            previous.length = previous
                .length
                .checked_add(mapping.length)
                .ok_or_else(Ext4Error::overflow)?;
            return Ok(());
        }
    }
    output.push(mapping);
    Ok(())
}
