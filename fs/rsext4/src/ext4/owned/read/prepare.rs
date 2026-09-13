//! Short snapshot preparation and current-inode metadata completion.

use super::*;
use crate::{ForkBlockIo, file::MAX_READ_BYTES};

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Prepares a retained inode's file read without inode-table or extent I/O.
    ///
    /// Keep its allocation and content protection through every returned phase.
    /// Oversized/overflowing requests, legacy/non-regular mappings and explicit
    /// UnsupportedCapability use Serialized; other errors remain explicit.
    pub fn prepare_inode_read(
        &mut self,
        number: InodeNumber,
        offset: u64,
        length: usize,
    ) -> Ext4Result<InodeReadPreparation<D>>
    where
        D: ForkBlockIo,
    {
        self.ensure_mounted("inode:prepare_read")?;
        self.writes.ensure_inode_idle(number)?;
        if length == 0 {
            return Ok(InodeReadPreparation::Empty);
        }
        let Some(end) = offset.checked_add(length as u64) else {
            return Ok(InodeReadPreparation::Serialized);
        };
        if length > MAX_READ_BYTES {
            return Ok(InodeReadPreparation::Serialized);
        }
        if let Some(cached) = self.filesystem.inodetable_cache.get_mut(number) {
            return self.prepare_file_snapshot(number, cached.inode, offset..end);
        }
        Ok(match self.prepare_uncached_live_inode_read(number)? {
            Some(prepared) => InodeReadPreparation::Inode(prepared),
            None => InodeReadPreparation::Serialized,
        })
    }

    /// Validates a cold retained inode and prepares its file read without dirty
    /// eviction or mapping I/O. None requests a new attempt after invalidation.
    /// Use the same request offset/length and protection as initial preparation.
    pub fn finish_read_inode_load(
        &mut self,
        completed: CompletedLiveInodeRead,
        offset: u64,
        length: usize,
    ) -> Ext4Result<Option<InodeReadPreparation<D>>>
    where
        D: ForkBlockIo,
    {
        let Some((number, inode)) = self.finish_live_inode_record(completed)? else {
            return Ok(None);
        };
        self.writes.ensure_inode_idle(number)?;
        if length == 0 {
            return Ok(Some(InodeReadPreparation::Empty));
        }
        let Some(end) = offset.checked_add(length as u64) else {
            return Ok(Some(InodeReadPreparation::Serialized));
        };
        if length > MAX_READ_BYTES {
            return Ok(Some(InodeReadPreparation::Serialized));
        }
        self.prepare_file_snapshot(number, inode, offset..end)
            .map(Some)
    }

    /// Validates bytes/errors and completes atime without copying file contents.
    ///
    /// None requests a new read after invalidation. Current read errors and
    /// insufficient output capacity are returned before atime changes. Existing
    /// journal-progress errors can retry this borrowed completion; invalidation
    /// during that progress instead requests fresh data. The returned view
    /// borrows only completed bytes and may be copied after releasing mount state.
    pub fn finish_inode_read<'a>(
        &mut self,
        completed: &'a CompletedInodeRead,
        output_capacity: usize,
    ) -> Ext4Result<Option<ValidatedInodeRead<'a>>> {
        if !self.read_snapshot_is_current(&completed.snapshot)? {
            return Ok(None);
        }
        let bytes = completed.result.as_ref().map_err(|error| *error)?;
        if output_capacity < bytes.len() {
            return Err(Ext4Error::buffer_too_small(output_capacity, bytes.len()));
        }
        if bytes.len() != 0 && !self.options.readonly {
            self.filesystem
                .touch_inode_atime_if_needed(&mut self.device, bytes.inode())?;
        }
        Ok(Some(ValidatedInodeRead { bytes }))
    }

    fn prepare_file_snapshot(
        &mut self,
        number: InodeNumber,
        inode: Ext4Inode,
        range: Range<u64>,
    ) -> Ext4Result<InodeReadPreparation<D>>
    where
        D: ForkBlockIo,
    {
        // Preserve EOF handling before the old non-regular/legacy fallback.
        if range.start < inode.size() && (!inode.is_file() || !inode.uses_extents()) {
            return Ok(InodeReadPreparation::Serialized);
        }
        Ok(match self.prepare_inode_blocks(number, inode)? {
            Some(blocks) => InodeReadPreparation::Read(PreparedInodeRead { blocks, range }),
            None => InodeReadPreparation::Serialized,
        })
    }
}
