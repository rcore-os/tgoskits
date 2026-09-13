//! Full-run writes and preservation of partial-block contents.

use super::*;

pub(super) fn write_inode_block_data<B: BlockIo>(
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

pub(super) fn write_full_block_run<B: BlockIo>(
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

pub(super) fn existing_full_block_run(
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
