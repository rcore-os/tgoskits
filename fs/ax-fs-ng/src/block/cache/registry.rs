//! Per-device cache sharing, mirroring Linux's single `address_space` per
//! block device: every partition-backed filesystem instance on one device
//! resolves to the same cache tree, keyed by the identity (allocation
//! address) of the runtime `BlockDeviceHandle`.
//!
//! Key stability holds because each cached device keeps its own `Arc`
//! clone of the handle alive, so the address cannot be reused while a
//! registry entry for it exists. Entries hold only weak references to the
//! tree: it dies with its last consumer after best-effort writeback in
//! `BufferedBlockDevice::drop`.
//!
//! Each entry also owns a device endpoint (an equivalent `FsBlockDevice`
//! over the same runtime handle) so global operations —
//! [`sync_all_block_caches`] — can write back trees that no wrapper is
//! currently driving.

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_lazyinit::LazyLock;

use super::{address_space::FolioGeometry, device::BlockCacheShared};
use crate::{BlockError, BlockResult, block::FsBlockDevice, os::sync::SleepMutex as Mutex};

struct DeviceCacheEntry {
    cache: Weak<BlockCacheShared>,
    /// Device endpoint for global writeback; equivalent to every wrapper's
    /// inner device because they share the same runtime handle.
    endpoint: Arc<Mutex<Box<dyn FsBlockDevice>>>,
}

static BLOCK_CACHE_REGISTRY: LazyLock<Mutex<BTreeMap<usize, DeviceCacheEntry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Resolves the shared cache tree of the device identified by
/// `device_key`, creating it on first use with `endpoint` as its global
/// writeback device.
///
/// # Errors
///
/// Returns [`BlockError::InvalidRequest`] when `block_size` is zero or not
/// a power of two, and [`BlockError::InvalidState`] when the registered
/// tree for `device_key` was built for a different block size (a key
/// collision that would mix incompatible folio layouts).
pub(crate) fn shared_cache_for(
    device_key: usize,
    block_size: usize,
    endpoint: Box<dyn FsBlockDevice>,
) -> BlockResult<Arc<BlockCacheShared>> {
    // Lock order: registry first, then the device tree lock. No path takes
    // them in the opposite order.
    let mut registry = BLOCK_CACHE_REGISTRY.lock();
    if let Some(entry) = registry.get(&device_key)
        && let Some(shared) = entry.cache.upgrade()
    {
        if shared.matches_block_size(block_size) {
            return Ok(shared);
        }
        return Err(BlockError::InvalidState);
    }

    let shared = Arc::new(BlockCacheShared::new(FolioGeometry::new(block_size)?));
    registry.retain(|_, entry| entry.cache.strong_count() > 0);
    registry.insert(
        device_key,
        DeviceCacheEntry {
            cache: Arc::downgrade(&shared),
            endpoint: Arc::new(Mutex::new(endpoint)),
        },
    );
    Ok(shared)
}

/// Writes back every live device cache tree and issues each device's flush
/// barrier (the `sync(2)` analog of `sync_dirty_buffers` across the
/// registry). Later devices are still attempted when one fails; the first
/// error is returned.
///
/// # Errors
///
/// Returns the first device writeback or flush error encountered.
pub fn sync_all_block_caches() -> BlockResult<()> {
    let mut first_error = None;
    for (shared, endpoint) in live_trees() {
        let result = shared.sync_to_device_with(&mut *endpoint.lock());
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Evicts up to `num_folios` clean folios across live device trees,
/// returning how many were dropped.
///
/// Only clean folios are reclaimed, matching the page-cache reclaim
/// contract: this runs from the allocator's pressure hook, where device
/// IO must not happen. Dirty folios are moved back to the
/// most-recently-used end and skipped.
///
/// Gated on `vfs` together with its only consumer, the page-cache
/// reclaim hook.
#[cfg(feature = "vfs")]
pub(crate) fn reclaim_clean_folios(num_folios: usize) -> usize {
    let mut reclaimed = 0;
    for (shared, _) in live_trees() {
        if reclaimed >= num_folios {
            break;
        }
        reclaimed += shared.reclaim_clean_folios(num_folios - reclaimed);
    }
    reclaimed
}

/// A live cache tree paired with its global-writeback endpoint.
type LiveTree = (Arc<BlockCacheShared>, Arc<DeviceEndpoint>);

/// The registry-owned device endpoint of one cache tree.
type DeviceEndpoint = Mutex<Box<dyn FsBlockDevice>>;

/// Collects live trees; the `Arc` endpoint keeps the device reachable
/// after the registry lock is released.
fn live_trees() -> Vec<LiveTree> {
    let registry = BLOCK_CACHE_REGISTRY.lock();
    registry
        .values()
        .filter_map(|entry| Some((entry.cache.upgrade()?, entry.endpoint.clone())))
        .collect()
}
