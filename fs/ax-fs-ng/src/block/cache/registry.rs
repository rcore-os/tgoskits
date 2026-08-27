//! Per-device cache sharing, mirroring Linux's single `address_space` per
//! block device: every partition-backed filesystem instance on one device
//! resolves to the same cache tree, keyed by the identity (allocation
//! address) of the runtime `BlockDeviceHandle`.
//!
//! Key stability holds for live entries because each cached device keeps its
//! own `Arc` clone of the handle alive. Entries hold only weak references to
//! the tree. The last filesystem consumer writes the tree back while it is
//! still upgradeable; the tree's final drop releases the endpoint and removes
//! its entry when the registry is immediately available. A contended drop may
//! leave a stale weak entry, which cache creation or global sync prunes before
//! it can be reused.
//!
//! Each live tree owns a device endpoint (an equivalent `FsBlockDevice`
//! over the same runtime handle) so global operations —
//! [`sync_all_block_caches`] — can write it back without extending the
//! endpoint lifetime beyond the tree's last consumer.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
    vec::Vec,
};
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_lazyinit::LazyLock;

use super::{address_space::FolioGeometry, device::BlockCacheShared};
use crate::{BlockError, BlockResult, block::FsBlockDevice, os::sync::SleepMutex as Mutex};

struct DeviceCacheEntry {
    device_key: usize,
    cache: Weak<BlockCacheShared>,
}

static BLOCK_CACHE_REGISTRY: LazyLock<Mutex<Vec<DeviceCacheEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
static FAIL_REGISTRY_RESERVE_FOR_KEY: AtomicUsize = AtomicUsize::new(0);

/// Resolves the shared cache tree of the device identified by
/// `device_key`, creating it on first use with `endpoint` as its global
/// writeback device.
///
/// # Errors
///
/// Returns [`BlockError::InvalidRequest`] when `block_size` is zero or not
/// a power of two, [`BlockError::InvalidState`] when the registered tree for
/// `device_key` was built for a different block size (a key collision that
/// would mix incompatible folio layouts), and [`BlockError::NoMemory`] when
/// the registry cannot grow.
pub(crate) fn shared_cache_for(
    device_key: usize,
    block_size: usize,
    endpoint: Box<dyn FsBlockDevice>,
) -> BlockResult<Arc<BlockCacheShared>> {
    let geometry = FolioGeometry::new(block_size)?;
    // Lock order: registry first, then the device tree lock. No path takes
    // them in the opposite order.
    let mut registry = BLOCK_CACHE_REGISTRY.lock();
    prune_stale_entries(&mut registry);
    let stale_index = if let Some(index) = registry
        .iter()
        .position(|entry| entry.device_key == device_key)
    {
        if let Some(shared) = registry[index].cache.upgrade() {
            if shared.matches_block_size(block_size) {
                return Ok(shared);
            }
            return Err(BlockError::InvalidState);
        }
        Some(index)
    } else {
        None
    };

    if stale_index.is_none() {
        #[cfg(test)]
        if FAIL_REGISTRY_RESERVE_FOR_KEY
            .compare_exchange(device_key, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Err(BlockError::NoMemory);
        }
        registry.try_reserve(1).map_err(|_| BlockError::NoMemory)?;
    }
    let shared = Arc::new(BlockCacheShared::new(device_key, geometry, endpoint));
    let entry = DeviceCacheEntry {
        device_key,
        cache: Arc::downgrade(&shared),
    };
    if let Some(index) = stale_index {
        registry[index] = entry;
    } else {
        registry.push(entry);
    }
    Ok(shared)
}

#[cfg(test)]
pub(crate) fn fail_registry_reserve_for_key_for_test(device_key: usize) {
    assert_ne!(device_key, 0, "zero means no injected allocation failure");
    FAIL_REGISTRY_RESERVE_FOR_KEY
        .compare_exchange(0, device_key, Ordering::AcqRel, Ordering::Acquire)
        .expect("only one registry allocation failure may be injected at a time");
}

#[cfg(test)]
pub(crate) fn registry_contains_key_for_test(device_key: usize) -> bool {
    BLOCK_CACHE_REGISTRY
        .lock()
        .iter()
        .any(|entry| entry.device_key == device_key)
}

/// Removes the registry entry that still names `cache` without waiting.
///
/// A contended drop leaves only a stale weak entry. Cache creation and global
/// sync prune such entries while they already own the registry lock.
pub(super) fn unregister_cache(device_key: usize, cache: *const BlockCacheShared) {
    let Some(mut registry) = BLOCK_CACHE_REGISTRY.try_lock() else {
        return;
    };
    let Some(index) = registry.iter().position(|entry| {
        entry.device_key == device_key && core::ptr::eq(entry.cache.as_ptr(), cache)
    }) else {
        return;
    };
    registry.swap_remove(index);
}

#[cfg(all(test, feature = "vfs"))]
pub(super) fn unregister_while_locked_for_test(device_key: usize, cache: *const BlockCacheShared) {
    let _registry = BLOCK_CACHE_REGISTRY.lock();
    unregister_cache(device_key, cache);
}

/// Writes back every live device cache tree and issues each device's flush
/// barrier (the `sync(2)` analog of `sync_dirty_buffers` across the
/// registry). Later devices are still attempted when one fails; the first
/// error is returned.
///
/// # Errors
///
/// Returns [`BlockError::NoMemory`] when the live-tree snapshot cannot be
/// reserved, otherwise the first device writeback or flush error encountered.
pub fn sync_all_block_caches() -> BlockResult<()> {
    let mut first_error = None;
    for shared in live_trees()? {
        let result = shared.sync_to_registered_device();
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
    let Some(tree_count) = BLOCK_CACHE_REGISTRY
        .try_lock()
        .map(|registry| registry.len())
    else {
        return 0;
    };
    let mut reclaimed = 0;
    for index in 0..tree_count {
        if reclaimed >= num_folios {
            break;
        }
        let Some(shared) = try_live_tree(index) else {
            continue;
        };
        reclaimed += shared.try_reclaim_clean_folios(num_folios - reclaimed);
    }
    reclaimed
}

/// Upgrades one tree without allocating or waiting for the registry lock.
///
/// The registry may change between indices. Reclaim is best-effort, so a
/// removed or contended entry is skipped; the returned strong reference is
/// dropped only after the registry guard has gone out of scope.
#[cfg(feature = "vfs")]
fn try_live_tree(index: usize) -> Option<Arc<BlockCacheShared>> {
    let registry = BLOCK_CACHE_REGISTRY.try_lock()?;
    registry.get(index)?.cache.upgrade()
}

fn prune_stale_entries(registry: &mut Vec<DeviceCacheEntry>) {
    registry.retain(|entry| entry.cache.strong_count() != 0);
}

/// Collects live trees; each tree owns the endpoint that keeps its device
/// reachable after the registry lock is released.
fn live_trees() -> BlockResult<Vec<Arc<BlockCacheShared>>> {
    let mut registry = BLOCK_CACHE_REGISTRY.lock();
    prune_stale_entries(&mut registry);
    let mut trees = Vec::new();
    trees
        .try_reserve_exact(registry.len())
        .map_err(|_| BlockError::NoMemory)?;
    for entry in registry.iter() {
        if let Some(shared) = entry.cache.upgrade() {
            trees.push(shared);
        }
    }
    Ok(trees)
}
