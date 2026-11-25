//! Preserve bytes outside a modified range or retained EOF block.

use super::*;

pub(super) fn zero_partial_mapped_blocks<B: BlockIo>(
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

pub(super) fn zero_mapped_inode_tail<B: BlockIo>(
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
