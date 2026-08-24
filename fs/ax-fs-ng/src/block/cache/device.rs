//! `FsBlockDevice` adapter routing traffic through the shared per-device
//! cache, the role the bdev page cache plays for Linux filesystem metadata
//! (`fs/buffer.c`).
//!
//! Requests inside one folio take the buffered path: reads are served
//! from folios when possible (`bread`), writes only mark slots dirty
//! (`mark_buffer_dirty`) and reach the device at writeback. Requests
//! spanning multiple folios take the device-direct path (the analog of
//! direct IO): overlapping dirty slots are written back first so the
//! device is current, the request is submitted unchanged, and the result
//! is overlaid onto cached folios. The buffered/direct split at folio
//! granularity replaces Linux's filesystem-declared metadata/data split
//! because the `FsBlockDevice` boundary only observes request shapes;
//! see the module documentation for the full mapping.

use alloc::{boxed::Box, sync::Arc};

use super::address_space::{BlockAddressSpace, FolioGeometry};
use crate::{BlockError, BlockResult, block::FsBlockDevice, os::sync::SleepMutex};

/// The shared per-device cache tree behind a sleepable lock.
///
/// All filesystem instances on one physical device serialize through this
/// lock (Linux instead locks folios individually; see module documentation
/// for why that split has no effect in the synchronous IO model).
pub(crate) struct BlockCacheShared {
    state: SleepMutex<BlockAddressSpace>,
}

impl BlockCacheShared {
    pub(crate) fn new(geometry: FolioGeometry) -> Self {
        Self {
            state: SleepMutex::new(BlockAddressSpace::new(geometry)),
        }
    }

    /// Whether the tree was built for `block_size`; a registry hit with a
    /// different size means the device key collides across geometries.
    pub(crate) fn matches_block_size(&self, block_size: usize) -> bool {
        self.state.lock().geometry().block_size() == block_size
    }

    /// Writes back every dirty slot through `endpoint`, then issues its
    /// flush barrier; used by global sync when no wrapper device drives
    /// the tree.
    pub(crate) fn sync_to_device_with(&self, endpoint: &mut dyn FsBlockDevice) -> BlockResult<()> {
        let mut state = self.state.lock();
        if state.has_dirty() {
            state.writeback_dirty(&mut *endpoint, None)?;
        }
        endpoint.flush()
    }

    /// Drops up to `target` clean folios from the LRU end; see
    /// [`super::registry::reclaim_clean_folios`] for the clean-only
    /// contract.
    #[cfg(feature = "vfs")]
    pub(crate) fn reclaim_clean_folios(&self, target: usize) -> usize {
        self.state.lock().reclaim_clean_folios(target)
    }
}

/// A [`FsBlockDevice`] wrapper whose buffered traffic is cached and shared
/// by every consumer of the same underlying device.
pub(crate) struct BufferedBlockDevice<T: FsBlockDevice> {
    inner: T,
    shared: Arc<BlockCacheShared>,
}

impl<T: FsBlockDevice> BufferedBlockDevice<T> {
    /// Wraps `inner`, resolving the shared cache tree registered under
    /// `device_key` (the identity of the runtime device handle);
    /// `endpoint` is an equivalent device the registry keeps for global
    /// writeback.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::InvalidRequest`] when the device block size is
    /// zero or not a power of two, and [`BlockError::InvalidState`] when
    /// the registered tree for `device_key` was built for a different
    /// block size.
    pub(crate) fn with_device_key(
        device_key: usize,
        endpoint: Box<dyn FsBlockDevice>,
        inner: T,
    ) -> BlockResult<Self> {
        let block_size = inner.block_size();
        let shared = super::registry::shared_cache_for(device_key, block_size, endpoint)?;
        Ok(Self { inner, shared })
    }

    /// Writes back every dirty slot, then issues the device flush barrier
    /// (`sync_dirty_buffers` followed by the cache flush). The barrier
    /// ordering is what keeps journal commit sequences crash-safe when
    /// block writes are deferred into this layer.
    fn sync_to_device(&mut self) -> BlockResult<()> {
        let mut state = self.shared.state.lock();
        if state.has_dirty() {
            state.writeback_dirty(&mut self.inner, None)?;
        }
        self.inner.flush()
    }

    /// Splits a request into `(first_block, block_count)`, validating the
    /// buffer geometry against the device block size.
    fn split_request(&self, block_id: u64, buf_len: usize) -> BlockResult<(u64, u64)> {
        let block_size = self.shared.state.lock().geometry().block_size();
        if block_size == 0 || buf_len == 0 || !buf_len.is_multiple_of(block_size) {
            return Err(BlockError::InvalidRequest);
        }
        let count = u64::try_from(buf_len / block_size).map_err(|_| BlockError::InvalidState)?;
        Ok((block_id, count))
    }
}

impl<T: FsBlockDevice> FsBlockDevice for BufferedBlockDevice<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult<()> {
        let (first, count) = self.split_request(block_id, buf.len())?;
        let mut state = self.shared.state.lock();
        if state.geometry().spans_one_folio(first, count) {
            return state.read_buffered(&mut self.inner, first, count, buf);
        }
        // Direct read: write overlapping dirty slots back first so stale
        // device bytes cannot bypass newer cached data.
        state.writeback_dirty(&mut self.inner, Some((first, count)))?;
        self.inner.read_block(block_id, buf)?;
        state.apply_direct(first, count, buf, true);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult<()> {
        let (first, count) = self.split_request(block_id, buf.len())?;
        let mut state = self.shared.state.lock();
        if state.geometry().spans_one_folio(first, count) {
            return state.write_buffered(&mut self.inner, first, count, buf);
        }
        // Direct write: the device must absorb overlapping dirty slots
        // before the newer direct bytes land, then the folios are overlaid.
        state.writeback_dirty(&mut self.inner, Some((first, count)))?;
        self.inner.write_block(block_id, buf)?;
        state.apply_direct(first, count, buf, false);
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.sync_to_device()
    }
}

impl<T: FsBlockDevice> Drop for BufferedBlockDevice<T> {
    fn drop(&mut self) {
        // The last user of a shared tree flushes pending writeback, like
        // `SeekableDisk::drop` does for its partial-block buffer. Races
        // between concurrent drops of the last two users can miss the
        // flush; callers that need durability must flush explicitly.
        if Arc::strong_count(&self.shared) == 1
            && let Err(error) = self.sync_to_device()
        {
            error!("failed to flush block cache while dropping device: {error:?}");
        }
    }
}
