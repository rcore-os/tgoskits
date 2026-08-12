use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use axfs_ng_vfs::VfsResult;

use super::{CachedFileShared, PageCache};

const MAX_RECLAIM_BATCH: usize = 256;

struct ReclaimGuard;

impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        RECLAIM_IN_PROGRESS.store(false, Ordering::Release);
    }
}

static GLOBAL_CACHED_FILES: ax_sync::SpinRwLock<Vec<Arc<CachedFileShared>>> =
    ax_sync::SpinRwLock::new(Vec::new());
static RECLAIM_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Reclaims clean disk-backed cache pages without holding listener callbacks
/// under the page-cache lock.
pub fn page_cache_reclaim(num_pages: usize) -> usize {
    if RECLAIM_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let _guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16) * 2;
    let mut file_count = 0;
    let Some(guard) = GLOBAL_CACHED_FILES.try_read() else {
        return 0;
    };
    let files = guard.clone();
    drop(guard);
    for file in &files {
        let freed = file.try_evict_clean_pages(target - reclaimed);
        reclaimed += freed;
        file_count += 1;
        if reclaimed >= target {
            break;
        }
    }

    if reclaimed > 0 {
        debug!(
            "page_cache_reclaim: evicted {} clean pages across {} files",
            reclaimed, file_count
        );
    }
    reclaimed
}

pub(super) fn register_cached_file(file: &Arc<CachedFileShared>) {
    prune_cached_files();
    let mut registry = GLOBAL_CACHED_FILES.write();
    if !registry
        .iter()
        .any(|registered| Arc::ptr_eq(registered, file))
    {
        registry.push(file.clone());
    }
}

#[cfg(feature = "ext4")]
pub(super) fn release_unlinked_cached_file(file: &Arc<CachedFileShared>) {
    // An unlinked inode is no longer a filesystem-global writeback owner. Open
    // file and mmap references retain their own page cache and can still use
    // explicit fsync/fdatasync. `file` keeps destruction outside the spin lock.
    GLOBAL_CACHED_FILES
        .write()
        .retain(|registered| !Arc::ptr_eq(registered, file));
}

pub fn sync_all_cached_files(_data_only: bool) -> VfsResult<()> {
    let files = GLOBAL_CACHED_FILES.read().clone();
    let mut first_error = None;
    for file in &files {
        if let Err(error) = file.writeback_dirty_for_global_sync()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }

    drop(files);
    prune_cached_files();
    first_error.map_or(Ok(()), Err)
}

fn prune_cached_files() {
    // Snapshot strong references so cache inspection and final destruction
    // happen without holding the registry spin lock. Recheck the strong count
    // under the lock before removing a clean, otherwise unowned entry.
    let files = GLOBAL_CACHED_FILES.read().clone();
    for cached in &files {
        if cached.has_dirty_pages() && !cached.is_unlinked() {
            continue;
        }
        let mut registry = GLOBAL_CACHED_FILES.write();
        if Arc::strong_count(cached) == 2
            && let Some(index) = registry
                .iter()
                .position(|registered| Arc::ptr_eq(registered, cached))
        {
            registry.swap_remove(index);
        }
    }
}

impl CachedFileShared {
    /// Scans the LRU and evicts up to `max` clean pages.
    ///
    /// The first phase removes candidates under `page_cache`; the second phase
    /// invokes mmap listeners after releasing that lock. A page is reinserted
    /// when any listener cannot invalidate its mapping.
    fn try_evict_clean_pages(&self, max: usize) -> usize {
        let limit = max.min(MAX_RECLAIM_BATCH);
        let mut pending: Vec<(u32, PageCache)> = Vec::new();
        {
            let Some(mut cache) = self.page_cache.try_lock() else {
                return 0;
            };
            let mut to_pop = [0u32; MAX_RECLAIM_BATCH];
            let mut count = 0;
            for (&pn, page) in cache.iter().rev() {
                if !page.dirty && count < limit {
                    to_pop[count] = pn;
                    count += 1;
                }
            }
            for &pn in &to_pop[..count] {
                if let Some(page) = cache.pop(&pn) {
                    pending.push((pn, page));
                }
            }
        }

        let mut evicted = 0;
        for (pn, page) in pending {
            let invalidated = self
                .evict_listeners
                .lock()
                .iter()
                .all(|listener| (listener.listener)(pn, &page));
            if invalidated {
                evicted += 1;
            } else {
                self.page_cache.lock().put(pn, page);
            }
        }
        evicted
    }
}

#[cfg(all(test, feature = "ext4"))]
mod tests {
    use super::*;

    #[test]
    fn global_registry_does_not_keep_unlinked_cached_file_alive() {
        let cached = Arc::new(CachedFileShared::new_unbounded(0));
        let lifetime = Arc::downgrade(&cached);
        register_cached_file(&cached);
        cached.mark_unlinked();
        release_unlinked_cached_file(&cached);
        drop(cached);

        assert!(
            lifetime.upgrade().is_none(),
            "the reclaim registry must not own the inode page cache"
        );
        prune_cached_files();
    }
}
