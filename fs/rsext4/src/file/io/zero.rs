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

/// Zeroes the retained EOF block's tail for good.
///
/// The callers zero bytes that the file has already lost or never shows: a
/// shrink whose new size is already on disk, or an extension that publishes
/// its larger size only after this succeeds. Nothing puts the bytes back.
/// A failed write-through can still leave the old bytes on the device; a
/// shrink then stays pending, and `finish_pending_truncate` zeroes them again
/// before any later write or preallocation can grow the file over them.
pub(super) fn zero_mapped_inode_tail<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    size: u64,
) -> Ext4Result<()> {
    let Some((physical, offset)) = mapped_inode_tail(device, fs, inode_num, inode, size)? else {
        return Ok(());
    };
    fs.datablock_cache
        .modify(device, physical, |block| block[offset..].fill(0))
}

/// The physical block and the offset in it where the retained EOF block's
/// tail starts, or `None` when `size` ends on a block boundary or in a hole.
fn mapped_inode_tail<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    size: u64,
) -> Ext4Result<Option<(AbsoluteBN, usize)>> {
    let block_bytes = fs.block_size() as u64;
    let tail_offset = size % block_bytes;
    if tail_offset == 0 {
        return Ok(None);
    }

    let logical = u32::try_from(size / block_bytes).map_err(|_| Ext4Error::file_too_large())?;
    let Some(physical) = resolve_inode_block(fs, device, inode_num, inode, logical)? else {
        return Ok(None);
    };
    let offset = usize::try_from(tail_offset).map_err(|_| Ext4Error::overflow())?;
    Ok(Some((physical, offset)))
}

/// The bytes that zeroing a retained EOF block replaced.
///
/// A shrink zeroes the tail of the block it keeps before it changes any
/// mapping, so a corrupt hidden branch still fails in the plan preflight.
/// When the metadata transaction that follows fails and rolls back, the file
/// keeps its old size, so these bytes are visible again and must be put back.
pub(super) struct ZeroedTail {
    physical: AbsoluteBN,
    offset: usize,
    bytes: Vec<u8>,
}

impl ZeroedTail {
    /// Writes the replaced bytes back into the cached block.
    pub(super) fn restore<B: BlockIo>(
        self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
    ) -> Ext4Result<()> {
        fs.datablock_cache.modify(device, self.physical, |block| {
            block[self.offset..].copy_from_slice(&self.bytes)
        })
    }

    /// Restores the replaced bytes after `error` failed the operation that
    /// zeroed them, and returns the error to report.
    ///
    /// That is `error` when the bytes are back, and the restore's own error
    /// when they are not, since the file has then lost them.
    fn restore_after<B: BlockIo>(
        self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        error: Ext4Error,
    ) -> Ext4Error {
        match self.restore(device, fs) {
            Ok(()) => error,
            Err(restore_error) => restore_error.with_operation("rollback:truncate_tail_restore"),
        }
    }
}

/// Zeroes the retained EOF block's tail and returns what it replaced.
///
/// Only a user shrink that commits in a single transaction uses this: until
/// that transaction commits, the file keeps its old size, so a failure must
/// put the bytes back.
pub(super) fn zero_mapped_inode_tail_with_undo<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    size: u64,
) -> Ext4Result<Option<ZeroedTail>> {
    let Some((physical, offset)) = mapped_inode_tail(device, fs, inode_num, inode, size)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    let mut zeroed = false;
    let zeroing = fs.datablock_cache.modify(device, physical, |block| {
        bytes.extend_from_slice(&block[offset..]);
        block[offset..].fill(0);
        zeroed = true;
    });
    let zeroed_tail = ZeroedTail {
        physical,
        offset,
        bytes,
    };
    match zeroing {
        Ok(()) => Ok(Some(zeroed_tail)),
        // The write-through can fail after the cached block was zeroed and
        // marked dirty, so the tail is put back before the error is returned.
        Err(error) if zeroed => Err(zeroed_tail.restore_after(device, fs, error)),
        Err(error) => Err(error),
    }
}

/// Puts a zeroed tail back when the shrink that zeroed it failed.
///
/// The shrink's own error is returned when the bytes are restored. When the
/// restore fails too, its error is returned instead, with the operation
/// `rollback:truncate_tail_restore`, so the caller knows the file lost the
/// bytes and not only that the shrink failed.
pub(super) fn restore_tail_on_error<B: BlockIo, T>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    zeroed_tail: Option<ZeroedTail>,
    result: Ext4Result<T>,
) -> Ext4Result<T> {
    match (result, zeroed_tail) {
        (Err(error), Some(zeroed_tail)) => Err(zeroed_tail.restore_after(device, fs, error)),
        (result, _) => result,
    }
}
