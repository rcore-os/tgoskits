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
use core::sync::atomic::{AtomicUsize, Ordering};

use super::address_space::{BlockAddressSpace, FolioGeometry};
use crate::{BlockError, BlockResult, block::FsBlockDevice, os::sync::SleepMutex};

/// The shared per-device cache tree and its global-writeback endpoint.
///
/// All filesystem instances on one physical device serialize through this
/// lock (Linux instead locks folios individually; see module documentation
/// for why that split has no effect in the synchronous IO model). The
/// independent consumer count excludes temporary global-sync references and
/// elects exactly one last wrapper to perform drop-time writeback.
pub(crate) struct BlockCacheShared {
    device_key: usize,
    consumers: AtomicUsize,
    state: SleepMutex<BlockAddressSpace>,
    endpoint: SleepMutex<Box<dyn FsBlockDevice>>,
}

impl BlockCacheShared {
    pub(crate) fn new(
        device_key: usize,
        geometry: FolioGeometry,
        endpoint: Box<dyn FsBlockDevice>,
    ) -> Self {
        Self {
            device_key,
            consumers: AtomicUsize::new(0),
            state: SleepMutex::new(BlockAddressSpace::new(geometry)),
            endpoint: SleepMutex::new(endpoint),
        }
    }

    /// Whether the tree was built for `block_size`; a registry hit with a
    /// different size means the device key collides across geometries.
    pub(crate) fn matches_block_size(&self, block_size: usize) -> bool {
        self.state.lock().geometry().block_size() == block_size
    }

    fn acquire_consumer(&self) -> BlockResult<()> {
        self.consumers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| BlockError::InvalidState)
    }

    fn release_consumer(&self) -> bool {
        let previous = self
            .consumers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .expect("each cache wrapper acquires exactly one consumer reference");
        previous == 1
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

    /// Writes back through the endpoint owned by the shared cache tree.
    pub(crate) fn sync_to_registered_device(&self) -> BlockResult<()> {
        self.sync_to_device_with(&mut **self.endpoint.lock())
    }

    /// Drops up to `target` clean folios from the LRU end; see
    /// [`super::registry::reclaim_clean_folios`] for the clean-only
    /// contract.
    #[cfg(feature = "vfs")]
    pub(crate) fn try_reclaim_clean_folios(&self, target: usize) -> usize {
        let Some(mut state) = self.state.try_lock() else {
            return 0;
        };
        state.reclaim_clean_folios(target)
    }
}

impl Drop for BlockCacheShared {
    fn drop(&mut self) {
        super::registry::unregister_cache(
            self.device_key,
            core::ptr::from_ref::<BlockCacheShared>(self),
        );
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
    /// zero or not a power of two, [`BlockError::InvalidState`] when the
    /// registered tree for `device_key` was built for a different block
    /// size, and [`BlockError::NoMemory`] when the registry cannot grow.
    pub(crate) fn with_device_key(
        device_key: usize,
        endpoint: Box<dyn FsBlockDevice>,
        inner: T,
    ) -> BlockResult<Self> {
        let block_size = inner.block_size();
        let shared = super::registry::shared_cache_for(device_key, block_size, endpoint)?;
        shared.acquire_consumer()?;
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

    #[cfg(all(test, feature = "vfs"))]
    pub(super) fn reclaim_from_allocator_while_state_locked_for_test(&self) -> usize {
        let _state = self.shared.state.lock();
        super::registry::reclaim_clean_folios(usize::MAX)
    }

    #[cfg(all(test, feature = "vfs"))]
    pub(super) fn unregister_while_registry_locked_for_test(&self) {
        super::registry::unregister_while_locked_for_test(
            self.shared.device_key,
            Arc::as_ptr(&self.shared),
        );
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

    #[cfg(feature = "ext4")]
    fn physical_block_size(&self) -> usize {
        self.inner.physical_block_size()
    }

    #[cfg(feature = "ext4")]
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    #[cfg(feature = "ext4")]
    fn supports_flush(&self) -> bool {
        self.inner.supports_flush()
    }

    #[cfg(feature = "ext4")]
    fn supports_fua(&self) -> bool {
        self.inner.supports_fua()
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
        match self.inner.write_block(block_id, buf) {
            Ok(()) => {
                state.apply_direct(first, count, buf, false);
                Ok(())
            }
            Err(error) => {
                // The device contract reports no completed prefix. Some
                // blocks may already be durable, so every overlapping folio
                // must be refetched before it can become authoritative again.
                state.invalidate_range(first, count);
                Err(error)
            }
        }
    }

    #[cfg(feature = "ext4")]
    fn write_block_fua(&mut self, block_id: u64, buf: &[u8]) -> BlockResult<()> {
        let (first, count) = self.split_request(block_id, buf.len())?;
        let mut state = self.shared.state.lock();

        // FUA is a durability request, never a deferred buffered write. Older
        // dirty bytes in the same range must reach the device first; then the
        // FUA request is sent unchanged and the shared cache is refreshed from
        // the completed image.
        state.writeback_dirty(&mut self.inner, Some((first, count)))?;
        match self.inner.write_block_fua(block_id, buf) {
            Ok(()) => {
                state.apply_direct(first, count, buf, false);
                Ok(())
            }
            Err(error) => {
                state.invalidate_range(first, count);
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.sync_to_device()
    }
}

impl<T: FsBlockDevice> Drop for BufferedBlockDevice<T> {
    fn drop(&mut self) {
        // The consumer count is independent from temporary strong refs held
        // by global sync. Exactly one concurrent wrapper drop observes the
        // transition to zero and flushes while the tree remains upgradeable,
        // so a same-key creator cannot race a stale tree against a new one.
        if self.shared.release_consumer()
            && let Err(error) = self.sync_to_device()
        {
            error!("failed to flush block cache while dropping device: {error:?}");
        }
    }
}
