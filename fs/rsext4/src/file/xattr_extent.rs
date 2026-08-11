//! Extended-attribute storage inspection for FIEMAP.

use alloc::vec::Vec;

use super::extent_map::{
    FileExtent, FileExtentMap, FileExtentState, finish_extent_map, maximum_inode_bytes,
    push_overlapping_extent,
};
use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    endian::{read_u16_le, read_u32_le},
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
};

const XATTR_MAGIC: u32 = 0xea02_0000;
const XATTR_IBODY_HEADER_SIZE: usize = 4;
const XATTR_ENTRY_SIZE: usize = 16;
const XATTR_TERMINATOR_SIZE: usize = 4;
const XATTR_SIZE_MAX: u32 = 1 << 24;

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
    let (block, inode_offset, _) = filesystem.inodetable_cache.calc_inode_location(
        inode_number,
        filesystem.superblock.s_inodes_per_group,
        AbsoluteBN::new(inode_table_start),
        filesystem.block_size(),
    )?;
    let mut block_bytes = alloc::vec![0u8; filesystem.block_size()];
    device.read_blocks(&mut block_bytes, block, 1)?;
    let inode_end = inode_offset
        .checked_add(inode_size)
        .ok_or_else(Ext4Error::overflow)?;
    let inode_bytes = block_bytes
        .get(inode_offset..inode_end)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_inode_bounds"))?;
    let xattr_bytes = inode_bytes
        .get(xattr_offset..)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_header_bounds"))?;
    if read_u32_le(&xattr_bytes[..XATTR_IBODY_HEADER_SIZE]) != XATTR_MAGIC {
        return Ok(None);
    }

    validate_inline_xattrs(&filesystem.superblock, filesystem.root_inode, xattr_bytes)?;
    Ok(Some((block, xattr_offset, inode_size - xattr_offset)))
}

fn validate_inline_xattrs(
    superblock: &Ext4Superblock,
    root_inode: InodeNumber,
    xattrs: &[u8],
) -> Ext4Result<()> {
    if xattrs.len() < XATTR_IBODY_HEADER_SIZE + XATTR_TERMINATOR_SIZE
        || read_u32_le(&xattrs[..XATTR_IBODY_HEADER_SIZE]) != XATTR_MAGIC
    {
        return Err(Ext4Error::corrupted().with_operation("inode:xattr_header"));
    }

    let mut entry_offset = XATTR_IBODY_HEADER_SIZE;
    loop {
        let prefix_end = entry_offset
            .checked_add(XATTR_TERMINATOR_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let entry_prefix = xattrs
            .get(entry_offset..prefix_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_entry_bounds"))?;
        if read_u32_le(entry_prefix) == 0 {
            break;
        }
        let entry_end = entry_offset
            .checked_add(XATTR_ENTRY_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let entry = xattrs
            .get(entry_offset..entry_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_entry_bounds"))?;
        let name_len = usize::from(entry[0]);
        let entry_length = round_up_4(
            XATTR_ENTRY_SIZE
                .checked_add(name_len)
                .ok_or_else(Ext4Error::overflow)?,
        )?;
        let next = entry_offset
            .checked_add(entry_length)
            .ok_or_else(Ext4Error::overflow)?;
        let name_end = entry_end
            .checked_add(name_len)
            .ok_or_else(Ext4Error::overflow)?;
        let name = xattrs
            .get(entry_end..name_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_name_bounds"))?;
        if name.contains(&0)
            || next
                .checked_add(XATTR_TERMINATOR_SIZE)
                .is_none_or(|end| end > xattrs.len())
        {
            return Err(Ext4Error::corrupted().with_operation("inode:xattr_name"));
        }
        entry_offset = next;
    }
    let names_end = entry_offset
        .checked_add(XATTR_TERMINATOR_SIZE)
        .ok_or_else(Ext4Error::overflow)?;

    entry_offset = XATTR_IBODY_HEADER_SIZE;
    loop {
        let prefix_end = entry_offset
            .checked_add(XATTR_TERMINATOR_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let entry_prefix = xattrs
            .get(entry_offset..prefix_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_entry_bounds"))?;
        if read_u32_le(entry_prefix) == 0 {
            break;
        }
        let entry_end = entry_offset
            .checked_add(XATTR_ENTRY_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let entry = xattrs
            .get(entry_offset..entry_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:xattr_entry_bounds"))?;
        let name_len = usize::from(entry[0]);
        let value_offset = usize::from(read_u16_le(&entry[2..4]));
        let value_inode = read_u32_le(&entry[4..8]);
        let value_size = read_u32_le(&entry[8..12]);
        if value_size > XATTR_SIZE_MAX {
            return Err(Ext4Error::corrupted().with_operation("inode:xattr_value_size"));
        }
        if value_inode != 0 {
            if !superblock.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_EA_INODE)
                || value_inode == root_inode.raw()
                || value_inode < superblock.s_first_ino
                || value_inode > superblock.s_inodes_count
                || value_size == 0
            {
                return Err(Ext4Error::corrupted().with_operation("inode:xattr_value_inode"));
            }
        } else if value_size != 0 {
            let value_size = usize::try_from(value_size).map_err(|_| Ext4Error::overflow())?;
            let padded_size = round_up_4(value_size)?;
            let value_end = value_offset
                .checked_add(value_size)
                .ok_or_else(Ext4Error::overflow)?;
            let padded_end = value_offset
                .checked_add(padded_size)
                .ok_or_else(Ext4Error::overflow)?;
            if value_offset < names_end || value_end > xattrs.len() || padded_end > xattrs.len() {
                return Err(Ext4Error::corrupted().with_operation("inode:xattr_value_bounds"));
            }
        }
        entry_offset = entry_offset
            .checked_add(round_up_4(
                XATTR_ENTRY_SIZE
                    .checked_add(name_len)
                    .ok_or_else(Ext4Error::overflow)?,
            )?)
            .ok_or_else(Ext4Error::overflow)?;
    }
    Ok(())
}

fn round_up_4(value: usize) -> Ext4Result<usize> {
    value
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or_else(Ext4Error::overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_xattr_fixture() -> Vec<u8> {
        let mut bytes = alloc::vec![0u8; 64];
        bytes[..4].copy_from_slice(&XATTR_MAGIC.to_le_bytes());
        bytes[4] = 3;
        bytes[5] = 1;
        bytes[6..8].copy_from_slice(&28u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
        bytes[20..23].copy_from_slice(b"key");
        bytes[28..32].copy_from_slice(b"data");
        bytes
    }

    fn xattr_superblock() -> Ext4Superblock {
        Ext4Superblock {
            s_first_ino: 11,
            s_inodes_count: 128,
            ..Ext4Superblock::default()
        }
    }

    #[test]
    fn checked_inline_xattr_accepts_linux_layout() {
        validate_inline_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &inline_xattr_fixture(),
        )
        .expect("valid inline xattr");
    }

    #[test]
    fn checked_inline_xattr_rejects_value_overlapping_names() {
        let mut bytes = inline_xattr_fixture();
        bytes[6..8].copy_from_slice(&24u16.to_le_bytes());
        let error = validate_inline_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &bytes,
        )
        .expect_err("overlapping xattr value must be rejected");
        assert_eq!(error.kind(), crate::error::Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn checked_inline_xattr_rejects_embedded_name_terminator() {
        let mut bytes = inline_xattr_fixture();
        bytes[21] = 0;
        let error = validate_inline_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &bytes,
        )
        .expect_err("embedded xattr name terminator must be rejected");
        assert_eq!(error.kind(), crate::error::Ext4ErrorKind::Corrupted);
    }
}
