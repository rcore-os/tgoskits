//! Per-device folio tree of the block cache, modeled on the
//! `address_space` of a Linux block-device inode (`block/bdev.c`).
//!
//! The tree maps folio frame index to [`CacheFolio`] (Linux: XArray page
//! index to folio). An ordered frame index records the frame-level DIRTY
//! mark (Linux `PAGECACHE_TAG_DIRTY`): "does the device have pending
//! writeback" is answerable in O(1), and writeback always visits frames in
//! ascending block order for deterministic device-visible ordering.
//!
//! A WRITEBACK mark has no state here: writeback is synchronous under the
//! device lock, so an intermediate mark would never be observed. It is
//! the extension point if writeback becomes asynchronous.

use alloc::vec::Vec;
use core::num::NonZeroUsize;

use super::{folio::CacheFolio, folio_cache::FolioCache};
use crate::{BlockError, BlockResult, block::FsBlockDevice, os::memory::PAGE_SIZE};

/// Folios cached per device: 1024 frames of 4 KiB = 4 MiB with 512-byte
/// device blocks.
pub(crate) const BLOCK_CACHE_FOLIO_CAP: usize = 1024;

/// Fixed folio layout of one device: folio size, device block size, and
/// the number of device blocks each folio covers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FolioGeometry {
    block_size: usize,
    folio_size: usize,
    slots: usize,
    slots_log2: u32,
}

impl FolioGeometry {
    /// Computes the folio layout for a device.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::InvalidRequest`] if `block_size` is zero or
    /// not a power of two.
    pub(crate) fn new(block_size: usize) -> BlockResult<Self> {
        if block_size == 0 || !block_size.is_power_of_two() {
            return Err(crate::BlockError::InvalidRequest);
        }
        // Folios are page-sized so a frame can back file-level page-cache
        // IO, but never smaller than one device block.
        let folio_size = PAGE_SIZE.max(block_size);
        let slots = folio_size / block_size;
        Ok(Self {
            block_size,
            folio_size,
            slots,
            slots_log2: slots.trailing_zeros(),
        })
    }

    pub(crate) fn block_size(&self) -> usize {
        self.block_size
    }

    pub(crate) fn folio_size(&self) -> usize {
        self.folio_size
    }

    pub(crate) fn slots(&self) -> usize {
        self.slots
    }

    fn frame_of(&self, block: u64) -> u64 {
        block >> self.slots_log2
    }

    fn slot_of(&self, block: u64) -> usize {
        (block & (self.slots - 1) as u64) as usize
    }

    fn frame_base_block(&self, frame: u64) -> u64 {
        frame << self.slots_log2
    }

    /// Whether the block range `[first, first + count)` stays within one
    /// folio, i.e. qualifies for the buffered path.
    pub(crate) fn spans_one_folio(&self, first: u64, count: u64) -> bool {
        match count
            .checked_sub(1)
            .and_then(|last| first.checked_add(last))
        {
            Some(last) => self.frame_of(first) == self.frame_of(last),
            None => false,
        }
    }
}

/// The cached-folio tree of one device (the bdev `address_space`).
pub(crate) struct BlockAddressSpace {
    geometry: FolioGeometry,
    folios: FolioCache,
    /// Frame-level DIRTY mark: frames with at least one dirty slot.
    /// Kept sorted so writeback order is deterministic. Capacity growth is
    /// reserved before any folio state is changed.
    dirty_frames: Vec<u64>,
}

impl BlockAddressSpace {
    pub(crate) fn new(geometry: FolioGeometry) -> Self {
        Self::with_capacity(geometry, BLOCK_CACHE_FOLIO_CAP)
    }

    /// Builds a tree with an explicit frame capacity (used by tests to
    /// exercise LRU eviction deterministically).
    pub(crate) fn with_capacity(geometry: FolioGeometry, capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity is clamped to >= 1");
        Self {
            geometry,
            folios: FolioCache::new(capacity),
            dirty_frames: Vec::new(),
        }
    }

    pub(crate) fn geometry(&self) -> FolioGeometry {
        self.geometry
    }

    pub(crate) fn has_dirty(&self) -> bool {
        !self.dirty_frames.is_empty()
    }

    /// Drops up to `target` clean folios from the LRU end and returns how
    /// many were dropped. Dirty folios are pushed back to the
    /// most-recently-used end: reclaim runs from the allocator's pressure
    /// hook, where device IO must not happen.
    #[cfg(feature = "vfs")]
    pub(crate) fn reclaim_clean_folios(&mut self, target: usize) -> usize {
        let mut reclaimed = 0;
        let mut dirty_skips = 0;
        while reclaimed < target {
            let len = self.folios.len();
            if len == 0 || dirty_skips >= len {
                break;
            }
            let Some(frame) = self.folios.least_recent() else {
                break;
            };
            if self
                .folios
                .get(&frame)
                .is_some_and(CacheFolio::has_dirty_slots)
            {
                self.folios.touch(frame);
                dirty_skips += 1;
                continue;
            }
            self.folios.remove(&frame);
            reclaimed += 1;
        }
        reclaimed
    }

    /// Buffered read of a one-folio request (`bread` semantics): slots that
    /// are uptodate are copied out without IO, missing ones are read from
    /// the device into the folio as merged runs.
    pub(crate) fn read_buffered<T: FsBlockDevice>(
        &mut self,
        dev: &mut T,
        first_block: u64,
        count: u64,
        out: &mut [u8],
    ) -> BlockResult<()> {
        let geometry = self.geometry;
        let frame = geometry.frame_of(first_block);
        let first_slot = geometry.slot_of(first_block);
        let count = usize::try_from(count).map_err(|_| crate::BlockError::InvalidRequest)?;
        let folio = self.getblk(dev, frame)?;
        fill_missing_slots(dev, folio, &geometry, frame, first_slot, count)?;
        folio.copy_from_slots(first_slot, count, out);
        Ok(())
    }

    /// Deferred write of a one-folio request (`mark_buffer_dirty`
    /// semantics): data lands only in the folio; the device copy is
    /// updated by [`writeback_dirty`](Self::writeback_dirty).
    pub(crate) fn write_buffered<T: FsBlockDevice>(
        &mut self,
        dev: &mut T,
        first_block: u64,
        count: u64,
        src: &[u8],
    ) -> BlockResult<()> {
        let geometry = self.geometry;
        let frame = geometry.frame_of(first_block);
        let first_slot = geometry.slot_of(first_block);
        let count = usize::try_from(count).map_err(|_| crate::BlockError::InvalidRequest)?;
        if self.dirty_frames.binary_search(&frame).is_err() {
            self.dirty_frames
                .try_reserve(1)
                .map_err(|_| BlockError::NoMemory)?;
        }
        let newly_dirty = {
            let folio = self.getblk(dev, frame)?;
            folio.copy_into_slots(first_slot, count, src);
            folio.mark_slots_dirty(first_slot, count)
        };
        if newly_dirty > 0
            && let Err(index) = self.dirty_frames.binary_search(&frame)
        {
            self.dirty_frames.insert(index, frame);
        }
        Ok(())
    }

    /// Writes back dirty slots (`sync_dirty_buffers`). Every run of
    /// consecutive dirty slots becomes one merged device write; frames are
    /// visited in ascending order. `range` restricts writeback to frames
    /// overlapping the block range.
    pub(crate) fn writeback_dirty<T: FsBlockDevice + ?Sized>(
        &mut self,
        dev: &mut T,
        range: Option<(u64, u64)>,
    ) -> BlockResult<()> {
        let frame_range = range.and_then(|(first, count)| {
            let last = count.checked_sub(1).and_then(|n| first.checked_add(n))?;
            Some((self.geometry.frame_of(first), self.geometry.frame_of(last)))
        });
        loop {
            let target = match frame_range {
                None if range.is_some() => None,
                None => self.dirty_frames.first().copied(),
                Some((first, last)) => {
                    let index = self.dirty_frames.partition_point(|frame| *frame < first);
                    self.dirty_frames
                        .get(index)
                        .copied()
                        .filter(|frame| *frame <= last)
                }
            };
            let Some(frame) = target else {
                break;
            };
            self.writeback_folio(dev, frame)?;
        }
        Ok(())
    }

    /// Overlays a device-direct request result onto overlapping folios so
    /// cached slots stay coherent with the bytes the device just saw.
    /// Dirty slots keep their newer bytes when `preserve_dirty` is set
    /// (direct reads); direct writes clear dirty state beforehand.
    pub(crate) fn apply_direct(
        &mut self,
        first: u64,
        count: u64,
        data: &[u8],
        preserve_dirty: bool,
    ) {
        let geometry = self.geometry;
        let Some(last) = count.checked_sub(1).and_then(|n| first.checked_add(n)) else {
            return;
        };
        for frame in geometry.frame_of(first)..=geometry.frame_of(last) {
            let Some(folio) = self.folios.get_mut(&frame) else {
                continue;
            };
            let (slot_lo, slot_hi) = overlap_slots(&geometry, frame, first, last);
            let data_begin = (geometry.frame_base_block(frame) + slot_lo as u64 - first) as usize
                * geometry.block_size;
            let data_end = data_begin + (slot_hi - slot_lo) * geometry.block_size;
            folio.overlay_external(
                slot_lo,
                slot_hi - slot_lo,
                &data[data_begin..data_end],
                preserve_dirty,
            );
        }
    }

    /// Discards every cached folio overlapping a device-direct request.
    ///
    /// A failed write does not report its completed prefix, so none of the
    /// overlapping cache bytes can remain authoritative. Whole folios are
    /// discarded even when only some slots overlap; retaining neighboring
    /// slots would require tracking an error-completion bitmap that the
    /// current [`FsBlockDevice`] contract does not expose.
    pub(crate) fn invalidate_range(&mut self, first: u64, count: u64) {
        let Some(last) = count.checked_sub(1).and_then(|n| first.checked_add(n)) else {
            return;
        };
        for frame in self.geometry.frame_of(first)..=self.geometry.frame_of(last) {
            self.clear_dirty_frame(frame);
            self.folios.remove(&frame);
        }
    }

    /// Writes back one dirty folio: merged dirty-slot runs become device
    /// writes; on success the slot state is clean again.
    fn writeback_folio<T: FsBlockDevice + ?Sized>(
        &mut self,
        dev: &mut T,
        frame: u64,
    ) -> BlockResult<()> {
        let geometry = self.geometry;
        let Some(folio) = self.folios.get_mut(&frame) else {
            self.clear_dirty_frame(frame);
            return Ok(());
        };
        let base = geometry.frame_base_block(frame);
        while let Some((slot, count)) = folio.dirty_runs().next() {
            let lba = base + slot as u64;
            dev.write_block(lba, folio.slot_bytes_mut(slot, count))?;
            folio.clear_dirty_slots(slot, count);
        }
        if !folio.has_dirty_slots() {
            self.clear_dirty_frame(frame);
        }
        Ok(())
    }

    /// Finds-or-allocates a folio (`getblk`). When the tree is full, the
    /// LRU folio is evicted; a dirty victim is written back before it is
    /// dropped so eviction never discards modifications.
    fn getblk<T: FsBlockDevice + ?Sized>(
        &mut self,
        dev: &mut T,
        frame: u64,
    ) -> BlockResult<&mut CacheFolio> {
        if self.folios.contains(&frame) {
            return Ok(self.folios.get_mut(&frame).expect("folio was found above"));
        }

        // Allocate before eviction: a failed folio allocation leaves the
        // existing cache contents and LRU order unchanged.
        let folio = CacheFolio::try_new(self.geometry.folio_size(), self.geometry.slots())?;
        self.folios.try_reserve_entry()?;
        if self.folios.is_full() {
            self.evict_lru(dev)?;
        }
        self.folios.insert_reserved(frame, folio);
        Ok(self
            .folios
            .get_mut(&frame)
            .expect("folio was just inserted or found above"))
    }

    fn evict_lru<T: FsBlockDevice + ?Sized>(&mut self, dev: &mut T) -> BlockResult<()> {
        let Some(frame) = self.folios.least_recent() else {
            return Ok(());
        };
        // A dirty victim must reach the device before its folio is dropped.
        if self.dirty_frames.binary_search(&frame).is_ok() {
            self.writeback_folio(dev, frame)?;
        }
        self.folios.remove(&frame);
        Ok(())
    }

    fn clear_dirty_frame(&mut self, frame: u64) {
        if let Ok(index) = self.dirty_frames.binary_search(&frame) {
            self.dirty_frames.remove(index);
        }
    }
}

/// Reads the not-yet-uptodate slots of a request region from the device
/// into the folio, merging consecutive missing blocks into single reads
/// (the per-buffer `submit_bh` loop of `block_read_full_folio`).
fn fill_missing_slots<T: FsBlockDevice>(
    dev: &mut T,
    folio: &mut CacheFolio,
    geometry: &FolioGeometry,
    frame: u64,
    first_slot: usize,
    count: usize,
) -> BlockResult<()> {
    let end = first_slot + count;
    let mut cursor = first_slot;
    while cursor < end {
        if folio.slot(cursor).is_uptodate() {
            cursor += 1;
            continue;
        }
        let run_start = cursor;
        while cursor < end && !folio.slot(cursor).is_uptodate() {
            cursor += 1;
        }
        let run_len = cursor - run_start;
        let lba = geometry.frame_base_block(frame) + run_start as u64;
        dev.read_block(lba, folio.slot_bytes_mut(run_start, run_len))?;
        folio.mark_slots_uptodate(run_start, run_len);
    }
    Ok(())
}

/// Slot range `[lo, hi)` of `frame` covered by block range
/// `[first, last]`.
fn overlap_slots(geometry: &FolioGeometry, frame: u64, first: u64, last: u64) -> (usize, usize) {
    let lo = if geometry.frame_of(first) == frame {
        geometry.slot_of(first)
    } else {
        0
    };
    let hi = if geometry.frame_of(last) == frame {
        geometry.slot_of(last) + 1
    } else {
        geometry.slots()
    };
    (lo, hi)
}
