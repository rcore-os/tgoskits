//! Per-device cache with short index exclusion and independently pinned folios.
//! Device I/O never holds the index lock. Both resident frames and writeback
//! snapshots consume the same bounded frame budget.

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use super::{
    folio::CacheFolio,
    folio_cache::FolioCache,
    folio_state::{FolioEntry, FolioPin, FrameBudget},
    range_locks::RangeLocks,
};
use crate::{
    BlockError, BlockResult,
    block::FsBlockDevice,
    os::{memory::PAGE_SIZE, sync::Mutex},
};

/// Folios cached per device: 1024 frames of 4 KiB = 4 MiB with 512-byte
/// device blocks.
pub(crate) const BLOCK_CACHE_FOLIO_CAP: usize = 1024;

const WRITEBACK_BATCH_BYTES: usize = 128 * 1024;

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

/// Shared cache index; entries remain pinned while their I/O runs unlocked.
pub(crate) struct BlockAddressSpace {
    geometry: FolioGeometry,
    folios: Mutex<FolioCache<Arc<FolioEntry>>>,
    budget: Arc<FrameBudget>,
    // Serialize misses only. Hits and unrelated folio writeback bypass it.
    insertion: Mutex<()>,
    ranges: RangeLocks,
}

impl BlockAddressSpace {
    pub(crate) fn new(geometry: FolioGeometry) -> Self {
        Self::with_capacity(geometry, BLOCK_CACHE_FOLIO_CAP)
    }

    pub(crate) fn with_capacity(geometry: FolioGeometry, capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity is clamped to >= 1");
        Self {
            geometry,
            folios: Mutex::new(FolioCache::new(capacity)),
            budget: FrameBudget::new(capacity.get()),
            insertion: Mutex::new(()),
            ranges: RangeLocks::new(),
        }
    }

    pub(crate) fn geometry(&self) -> FolioGeometry {
        self.geometry
    }

    #[cfg(test)]
    pub(crate) fn has_dirty(&self) -> bool {
        self.frames()
            .expect("test cache index snapshot")
            .into_iter()
            .any(|frame| {
                self.pin(frame)
                    .is_some_and(|pin| pin.entry.data.lock().has_dirty_slots())
            })
    }

    pub(crate) fn read_buffered<T: FsBlockDevice>(
        &self,
        dev: &mut T,
        first: u64,
        count: u64,
        out: &mut [u8],
    ) -> BlockResult<()> {
        let frame = self.geometry.frame_of(first);
        let _range = self.ranges.lock(frame, frame)?;
        let pin = self.getblk(dev, frame)?;
        let slot = self.geometry.slot_of(first);
        let count = usize::try_from(count).map_err(|_| BlockError::InvalidRequest)?;
        {
            let folio = pin.entry.data.lock();
            if (slot..slot + count).all(|slot| folio.slot(slot).is_uptodate()) {
                folio.copy_from_slots(slot, count, out);
                return Ok(());
            }
        }
        let _io = pin.entry.io.lock();
        let mut folio = pin.entry.data.lock();
        fill_missing_slots(dev, &mut folio, &self.geometry, frame, slot, count)?;
        folio.copy_from_slots(slot, count, out);
        Ok(())
    }

    pub(crate) fn write_buffered<T: FsBlockDevice>(
        &self,
        dev: &mut T,
        first: u64,
        count: u64,
        src: &[u8],
    ) -> BlockResult<()> {
        let frame = self.geometry.frame_of(first);
        let _range = self.ranges.lock(frame, frame)?;
        let pin = self.getblk(dev, frame)?;
        // Writes may redirty a folio while an immutable snapshot is in flight.
        let mut folio = pin.entry.data.lock();
        let slot = self.geometry.slot_of(first);
        let count = usize::try_from(count).map_err(|_| BlockError::InvalidRequest)?;
        folio.copy_into_slots(slot, count, src)?;
        folio.mark_slots_dirty(slot, count);
        Ok(())
    }

    /// Captures a finite frame set; later dirtying does not extend this pass.
    pub(crate) fn writeback_dirty<T: FsBlockDevice + ?Sized>(
        &self,
        dev: &mut T,
        range: Option<(u64, u64)>,
    ) -> BlockResult<()> {
        let bounds = match range {
            Some((first, count)) => Some(self.frame_bounds(first, count)?),
            None => None,
        };
        let frames = self.frames()?;
        let mut index = 0;
        while index < frames.len() {
            let frame = frames[index];
            if bounds.is_some_and(|(first, last)| frame < first || frame > last) {
                index += 1;
                continue;
            }
            let written = self.writeback_full_run(
                dev,
                &frames[index..],
                bounds.map_or(u64::MAX, |(_, last)| last),
            )?;
            if written != 0 {
                index += written;
                continue;
            }
            if let Some(pin) = self.pin(frame) {
                self.writeback_folio(dev, frame, &pin.entry)?;
            }
            index += 1;
        }
        Ok(())
    }

    /// Copies contiguous full folios under the same budget as individual
    /// snapshots. I/O owners are acquired in ascending frame order, while
    /// ordinary writes remain free to redirty the data behind the snapshot.
    fn writeback_full_run<T: FsBlockDevice + ?Sized>(
        &self,
        dev: &mut T,
        frames: &[u64],
        last: u64,
    ) -> BlockResult<usize> {
        let max_frames = WRITEBACK_BATCH_BYTES / self.geometry.folio_size();
        if max_frames < 2 {
            return Ok(0);
        }
        let mut pins = Vec::new();
        let mut permits = Vec::new();
        if pins.try_reserve_exact(max_frames).is_err()
            || permits.try_reserve_exact(max_frames).is_err()
        {
            return Ok(0);
        }
        for &frame in frames.iter().take(max_frames) {
            if frame > last || frames[0].checked_add(pins.len() as u64) != Some(frame) {
                break;
            }
            let Some(permit) = self.budget.acquire() else {
                break;
            };
            let Some(pin) = self.pin(frame) else {
                break;
            };
            if pin.entry.data.lock().dirty_runs().next() != Some((0, self.geometry.slots())) {
                break;
            }
            pins.push(pin);
            permits.push(permit);
        }
        if pins.len() < 2 {
            return Ok(0);
        }
        let mut owners = Vec::new();
        let mut generations = Vec::new();
        let mut bytes = Vec::new();
        if owners.try_reserve_exact(pins.len()).is_err()
            || generations.try_reserve_exact(pins.len()).is_err()
            || bytes
                .try_reserve_exact(pins.len() * self.geometry.folio_size())
                .is_err()
        {
            return Ok(0);
        }
        for pin in &pins {
            owners.push(pin.entry.io.lock());
            let mut folio = pin.entry.data.lock();
            if folio.dirty_runs().next() != Some((0, self.geometry.slots())) {
                return Ok(0);
            }
            generations.push(folio.dirty_generation());
            bytes.extend_from_slice(folio.slot_bytes_mut(0, self.geometry.slots()));
        }
        // On an indeterminate failure every generation remains dirty. The
        // permits outlive `bytes`, including this error path.
        dev.write_block(self.geometry.frame_base_block(frames[0]), &bytes)?;
        for (pin, generation) in pins.iter().zip(generations) {
            pin.entry
                .data
                .lock()
                .finish_writeback_generation(generation, 0, self.geometry.slots());
        }
        Ok(pins.len())
    }

    pub(crate) fn read_direct<T: FsBlockDevice>(
        &self,
        dev: &mut T,
        first: u64,
        count: u64,
        out: &mut [u8],
    ) -> BlockResult<()> {
        let (start, end) = self.frame_bounds(first, count)?;
        let _range = self.ranges.lock(start, end)?;
        self.writeback_dirty(dev, Some((first, count)))?;
        dev.read_block(first, out)?;
        self.apply_direct(first, count, out, true)?;
        Ok(())
    }

    pub(crate) fn write_direct<T: FsBlockDevice>(
        &self,
        dev: &mut T,
        first: u64,
        count: u64,
        src: &[u8],
        submit: impl FnOnce(&mut T, u64, &[u8]) -> BlockResult<()>,
    ) -> BlockResult<()> {
        let (start, end) = self.frame_bounds(first, count)?;
        let _range = self.ranges.lock(start, end)?;
        self.writeback_dirty(dev, Some((first, count)))?;
        match submit(dev, first, src) {
            Ok(()) => self.apply_direct(first, count, src, false),
            Err(error) => {
                self.invalidate_range(first, count)?;
                Err(error)
            }
        }
    }

    fn frame_bounds(&self, first: u64, count: u64) -> BlockResult<(u64, u64)> {
        let last = count
            .checked_sub(1)
            .and_then(|count| first.checked_add(count))
            .ok_or(BlockError::InvalidRequest)?;
        Ok((self.geometry.frame_of(first), self.geometry.frame_of(last)))
    }

    fn frames(&self) -> BlockResult<Vec<u64>> {
        let folios = self.folios.lock();
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(folios.len())
            .map_err(|_| BlockError::NoMemory)?;
        frames.extend(folios.frames());
        drop(folios);
        frames.sort_unstable();
        Ok(frames)
    }

    fn pin(&self, frame: u64) -> Option<FolioPin<'_>> {
        let entry = self.folios.lock().get_mut(&frame).cloned()?;
        Some(FolioPin::new(entry, &self.budget))
    }

    fn getblk<T: FsBlockDevice + ?Sized>(
        &self,
        dev: &mut T,
        frame: u64,
    ) -> BlockResult<FolioPin<'_>> {
        if let Some(pin) = self.pin(frame) {
            return Ok(pin);
        }
        let _insertion = self.insertion.lock();
        if let Some(pin) = self.pin(frame) {
            return Ok(pin);
        }
        self.folios.lock().try_reserve_entry()?;
        let permit = loop {
            if let Some(permit) = self.budget.acquire() {
                break permit;
            }
            if self.evict_one(dev)? {
                continue;
            }
            self.budget
                .waiters
                .wait_while(|| !self.budget.available() && !self.has_unpinned_folio())?;
        };
        // The permit is acquired before the backing frame is allocated.
        let folio = CacheFolio::try_new(self.geometry.folio_size(), self.geometry.slots())?;
        let entry = Arc::new(FolioEntry::new(folio, permit));
        self.folios
            .lock()
            .insert_reserved(frame, Arc::clone(&entry));
        Ok(FolioPin::new(entry, &self.budget))
    }

    fn has_unpinned_folio(&self) -> bool {
        let folios = self.folios.lock();
        folios.frames().any(|frame| {
            folios
                .get(&frame)
                .is_some_and(|entry| Arc::strong_count(entry) == 1)
        })
    }

    fn eviction_candidate(&self) -> Option<(u64, FolioPin<'_>)> {
        let mut folios = self.folios.lock();
        for _ in 0..folios.len() {
            let frame = folios.least_recent()?;
            let entry = folios.get(&frame)?;
            if Arc::strong_count(entry) == 1 {
                return Some((frame, FolioPin::new(Arc::clone(entry), &self.budget)));
            }
            folios.touch(frame);
        }
        None
    }

    fn evict_one<T: FsBlockDevice + ?Sized>(&self, dev: &mut T) -> BlockResult<bool> {
        let Some((frame, pin)) = self.eviction_candidate() else {
            return Ok(false);
        };
        self.writeback_folio(dev, frame, &pin.entry)?;
        let removed = {
            let mut folios = self.folios.lock();
            // An index lookup racing the writeback can pin or redirty it.
            let clean = Arc::strong_count(&pin.entry) == 2
                && pin
                    .entry
                    .data
                    .try_lock()
                    .is_some_and(|folio| !folio.has_dirty_slots());
            if clean {
                folios.remove(&frame)
            } else {
                folios.touch(frame);
                None
            }
        };
        let evicted = removed.is_some();
        drop(removed);
        drop(pin);
        Ok(evicted)
    }

    fn writeback_folio<T: FsBlockDevice + ?Sized>(
        &self,
        dev: &mut T,
        frame: u64,
        entry: &FolioEntry,
    ) -> BlockResult<()> {
        let _io = entry.io.lock();
        let mut folio = entry.data.lock();
        if !folio.has_dirty_slots() {
            return Ok(());
        }
        let base = self.geometry.frame_base_block(frame);
        if let Some(permit) = self.budget.acquire() {
            let mut snapshot = folio.snapshot()?;
            drop(folio);
            while let Some((slot, count)) = snapshot.dirty_runs().next() {
                dev.write_block(base + slot as u64, snapshot.slot_bytes_mut(slot, count))?;
                entry.data.lock().finish_writeback(&snapshot, slot, count);
                snapshot.clear_dirty_slots(slot, count);
            }
            drop(snapshot);
            drop(permit);
        } else {
            // A full budget cannot allocate a hidden extra frame. Write this
            // one folio in place; unrelated folios and index hits still run.
            while let Some((slot, count)) = folio.dirty_runs().next() {
                dev.write_block(base + slot as u64, folio.slot_bytes_mut(slot, count))?;
                folio.clear_dirty_slots(slot, count);
            }
        }
        Ok(())
    }

    fn apply_direct(
        &self,
        first: u64,
        count: u64,
        data: &[u8],
        preserve_dirty: bool,
    ) -> BlockResult<()> {
        let (start, end) = self.frame_bounds(first, count)?;
        let last = first + count - 1;
        for frame in start..=end {
            let Some(pin) = self.pin(frame) else {
                continue;
            };
            let (lo, hi) = overlap_slots(&self.geometry, frame, first, last);
            let begin = (self.geometry.frame_base_block(frame) + lo as u64 - first) as usize
                * self.geometry.block_size;
            let end = begin + (hi - lo) * self.geometry.block_size;
            pin.entry
                .data
                .lock()
                .overlay_external(lo, hi - lo, &data[begin..end], preserve_dirty);
        }
        Ok(())
    }

    fn invalidate_range(&self, first: u64, count: u64) -> BlockResult<()> {
        let (start, end) = self.frame_bounds(first, count)?;
        for frame in start..=end {
            if let Some(pin) = self.pin(frame) {
                // Range exclusion prevents dirtying/loading this folio until
                // all indeterminate device bytes have lost authority.
                pin.entry.data.lock().invalidate();
            }
        }
        Ok(())
    }

    #[cfg(feature = "vfs")]
    pub(crate) fn reclaim_clean_folios(&self, target: usize) -> usize {
        let mut reclaimed = 0;
        let mut skipped = 0;
        loop {
            let Some(mut folios) = self.folios.try_lock() else {
                break;
            };
            if reclaimed >= target || skipped >= folios.len() {
                break;
            }
            let Some(frame) = folios.least_recent() else {
                break;
            };
            let entry = folios.get(&frame).expect("LRU frame belongs to the index");
            let clean = Arc::strong_count(entry) == 1
                && entry
                    .data
                    .try_lock()
                    .is_some_and(|folio| !folio.has_dirty_slots());
            if !clean {
                folios.touch(frame);
                skipped += 1;
                continue;
            }
            let removed = folios.remove(&frame);
            drop(folios);
            drop(removed);
            reclaimed += 1;
        }
        reclaimed
    }

    #[cfg(all(test, feature = "vfs"))]
    pub(crate) fn reclaim_while_index_locked_for_test(&self) -> usize {
        let _index = self.folios.lock();
        super::registry::reclaim_clean_folios(usize::MAX)
    }

    #[cfg(test)]
    pub(super) fn allocated_frames(&self) -> usize {
        self.budget.used()
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
