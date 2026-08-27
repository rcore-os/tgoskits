//! Folio-sized frames of the block cache, modeled on a Linux `folio` with
//! an attached `buffer_head` chain (`fs/buffer.c: folio_create_buffers`).
//!
//! A [`CacheFolio`] owns one frame of device data plus the per-block state
//! of every slot inside it. Frame-level dirty state is always the
//! aggregation of slot bits, mirroring the Linux rule that a dirty page
//! with buffers only means "at least one block is dirty".

use alloc::vec::Vec;

use super::buffer_head::BufferHead;
use crate::{BlockError, BlockResult};

/// One frame of the block cache: the frame bytes plus per-slot states.
#[derive(Debug)]
pub(crate) struct CacheFolio {
    data: Vec<u8>,
    heads: Vec<BufferHead>,
}

impl CacheFolio {
    /// Allocates one folio and all of its per-block state.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::NoMemory`] when either backing allocation
    /// cannot be reserved. Both vectors remain local until they are fully
    /// initialized, so a partial allocation never enters the cache tree.
    pub(crate) fn try_new(folio_size: usize, slots: usize) -> BlockResult<Self> {
        let mut data = Vec::new();
        data.try_reserve_exact(folio_size)
            .map_err(|_| BlockError::NoMemory)?;
        data.resize(folio_size, 0);

        let mut heads = Vec::new();
        heads
            .try_reserve_exact(slots)
            .map_err(|_| BlockError::NoMemory)?;
        heads.resize(slots, BufferHead::default());

        Ok(Self { data, heads })
    }

    pub(crate) fn slot(&self, slot: usize) -> &BufferHead {
        &self.heads[slot]
    }

    /// Byte range covering `[slot, slot + count)` blocks of this folio.
    fn slot_bytes(&self, slot: usize, count: usize) -> core::ops::Range<usize> {
        let block = self.data.len() / self.heads.len();
        slot * block..(slot + count) * block
    }

    /// Mutable bytes of a slot range, used as the device IO target so reads
    /// and writebacks never stage through a second buffer.
    pub(crate) fn slot_bytes_mut(&mut self, slot: usize, count: usize) -> &mut [u8] {
        let range = self.slot_bytes(slot, count);
        &mut self.data[range]
    }

    pub(crate) fn copy_from_slots(&self, slot: usize, count: usize, dst: &mut [u8]) {
        let range = self.slot_bytes(slot, count);
        dst.copy_from_slice(&self.data[range]);
    }

    pub(crate) fn copy_into_slots(&mut self, slot: usize, count: usize, src: &[u8]) {
        let range = self.slot_bytes(slot, count);
        self.data[range].copy_from_slice(src);
    }

    pub(crate) fn mark_slots_uptodate(&mut self, slot: usize, count: usize) {
        for head in &mut self.heads[slot..slot + count] {
            head.mark_uptodate();
        }
    }

    /// Marks a slot range dirty (`mark_buffer_dirty`); returns how many
    /// slots were dirty already, so the caller can keep tree-level dirty
    /// accounting exact.
    pub(crate) fn mark_slots_dirty(&mut self, slot: usize, count: usize) -> usize {
        let mut newly_dirty = 0;
        for head in &mut self.heads[slot..slot + count] {
            if !head.is_dirty() {
                newly_dirty += 1;
            }
            head.mark_dirty();
        }
        newly_dirty
    }

    /// Clears dirty state of a slot range; returns how many slots were
    /// dirty before the call.
    pub(crate) fn clear_dirty_slots(&mut self, slot: usize, count: usize) -> usize {
        let mut cleared = 0;
        for head in &mut self.heads[slot..slot + count] {
            if head.clear_dirty() {
                cleared += 1;
            }
        }
        cleared
    }

    pub(crate) fn has_dirty_slots(&self) -> bool {
        self.heads.iter().any(BufferHead::is_dirty)
    }

    /// Overlays the result of a device-direct request onto this folio.
    ///
    /// `src` holds the bytes of the whole direct request starting at this
    /// folio's first overlapping slot. Dirty slots hold data newer than the
    /// device copy, so they keep their bytes when `preserve_dirty` is set
    /// (direct reads); direct writes pass `false` because any overlapping
    /// dirty state was written back before the device write.
    pub(crate) fn overlay_external(
        &mut self,
        slot: usize,
        count: usize,
        src: &[u8],
        preserve_dirty: bool,
    ) {
        let block = self.data.len() / self.heads.len();
        for (offset, head) in self.heads[slot..slot + count].iter_mut().enumerate() {
            let src_block = &src[offset * block..(offset + 1) * block];
            if preserve_dirty && head.is_dirty() {
                continue;
            }
            let start = (slot + offset) * block;
            self.data[start..start + block].copy_from_slice(src_block);
            head.mark_uptodate();
        }
    }

    /// Iterates runs of consecutive dirty slots in ascending order, so
    /// writeback can merge adjacent blocks into single device writes
    /// (the `__block_write_full_folio` rule: only dirty blocks are
    /// submitted, never whole folios).
    pub(crate) fn dirty_runs(&self) -> DirtyRuns<'_> {
        DirtyRuns {
            heads: &self.heads,
            cursor: 0,
        }
    }
}

/// Iterator over `(first_slot, block_count)` runs of consecutive dirty
/// slots.
pub(crate) struct DirtyRuns<'a> {
    heads: &'a [BufferHead],
    cursor: usize,
}

impl Iterator for DirtyRuns<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.heads[self.cursor..]
            .iter()
            .position(BufferHead::is_dirty)?
            + self.cursor;
        let end = self.heads[start..]
            .iter()
            .position(|head| !head.is_dirty())
            .map_or(self.heads.len(), |clean| start + clean);
        self.cursor = end;
        Some((start, end - start))
    }
}
