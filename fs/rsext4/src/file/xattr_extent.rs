//! Extended-attribute storage inspection for FIEMAP.

use alloc::vec::Vec;

use super::{
    extent_map::{
        FileExtent, FileExtentMap, FileExtentState, finish_extent_map, maximum_inode_bytes,
        push_overlapping_extent,
    },
    xattr::has_valid_inline_store,
};
use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    error::{Ext4Error, Ext4Result},
};

const XATTR_IBODY_HEADER_SIZE: usize = 4;
const XATTR_TERMINATOR_SIZE: usize = 4;

pub(super) fn inspect_xattr_extent<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
    start: u64,
    length: u64,
    extent_limit: usize,
) -> Ext4Result<FileExtentMap> {
    let maximum_file_bytes = maximum_inode_bytes(filesystem, inode.uses_extents())?;
    let query_end = start.saturating_add(length).min(maximum_file_bytes);
    let mut mappings = Vec::new();

    if let Some((block, xattr_offset, xattr_length)) =
        inline_xattr_location(device, filesystem, inode_number, inode)?
    {
        // Linux 7.1 intentionally reports the inode-table block base plus the
        // in-inode xattr offset. It does not add the inode slot offset inside
        // that block, so preserve that observable FIEMAP ABI exactly.
        let physical_start = block
            .raw()
            .checked_mul(filesystem.block_size() as u64)
            .and_then(|base| base.checked_add(xattr_offset as u64))
            .ok_or_else(Ext4Error::overflow)?;
        push_overlapping_extent(
            &mut mappings,
            start,
            query_end,
            FileExtent {
                logical_start: 0,
                physical_start,
                length: xattr_length as u64,
                state: FileExtentState::Inline,
                merged: false,
            },
        )?;
    } else {
        let xattr_block = inode.file_acl();
        if xattr_block != 0 {
            if xattr_block >= filesystem.superblock.blocks_count()
                || xattr_block >= device.total_blocks()
            {
                return Err(Ext4Error::corrupted().with_operation("inode:extent_query_xattr_block"));
            }
            let block_size = filesystem.block_size() as u64;
            let physical_start = xattr_block
                .checked_mul(block_size)
                .ok_or_else(Ext4Error::overflow)?;
            push_overlapping_extent(
                &mut mappings,
                start,
                query_end,
                FileExtent {
                    logical_start: 0,
                    physical_start,
                    length: block_size,
                    state: FileExtentState::Initialized,
                    merged: false,
                },
            )?;
        }
    }

    finish_extent_map(mappings, extent_limit)
}

fn inline_xattr_location<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<Option<(AbsoluteBN, usize, usize)>> {
    if inode.i_extra_isize == 0 {
        return Ok(None);
    }
    let inode_size = filesystem.inode_disk_size() as usize;
    let xattr_offset = usize::from(Ext4Inode::GOOD_OLD_INODE_SIZE)
        .checked_add(usize::from(inode.i_extra_isize))
        .ok_or_else(Ext4Error::overflow)?;
    let minimum_end = xattr_offset
        .checked_add(XATTR_IBODY_HEADER_SIZE + XATTR_TERMINATOR_SIZE)
        .ok_or_else(Ext4Error::overflow)?;
    if minimum_end > inode_size {
        return Ok(None);
    }

    let (group, _) = filesystem.inode_allocator.global_to_group(inode_number)?;
    let inode_table_start = filesystem
        .group_descs
        .get(group.as_usize()?)
        .ok_or_else(Ext4Error::corrupted)?
        .inode_table();
    let (block, _inode_offset, _) = filesystem.inodetable_cache.calc_inode_location(
        inode_number,
        filesystem.superblock.s_inodes_per_group,
        AbsoluteBN::new(inode_table_start),
        filesystem.block_size(),
    )?;
    let (_, inode_bytes) = filesystem.get_inode_record(device, inode_number)?;
    if inode_bytes.len() != inode_size {
        return Err(Ext4Error::corrupted().with_operation("inode:xattr_inode_bounds"));
    }
    if !has_valid_inline_store(filesystem, inode, &inode_bytes)? {
        return Ok(None);
    }
    Ok(Some((block, xattr_offset, inode_size - xattr_offset)))
}
