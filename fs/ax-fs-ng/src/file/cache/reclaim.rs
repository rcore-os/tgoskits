use alloc::{sync::Arc, vec::Vec};
use core::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

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
    let guard = ReclaimGuard;

    let mut reclaimed = 0;
    let target = num_pages.max(16) * 2;
    let mut visited_files = 0;
    let scan_len = {
        let Some(registry) = GLOBAL_CACHED_FILES.try_read() else {
            return 0;
        };
        registry.len()
    };

    // Clone one Arc at a time so file locks are taken after the registry spin
    // guard is released, without allocating a second Vec under memory pressure.
    // Concurrent pruning may move an entry between indices; reclaim is a
    // best-effort scan, so a later allocator retry can revisit a skipped entry.
    for index in 0..scan_len {
        let file = {
            let Some(registry) = GLOBAL_CACHED_FILES.try_read() else {
                break;
            };
            registry.get(index).cloned()
        };
        let Some(file) = file else {
            continue;
        };

        let freed = file.try_evict_clean_pages(target - reclaimed);
        reclaimed += freed;
        visited_files += 1;
        if reclaimed >= target {
            break;
        }
    }
    drop(guard);

    // The remaining quota goes to the block-layer cache trees; like the
    // page cache above, only clean folios are reclaimable here.
    #[cfg(any(feature = "ext4", feature = "fat"))]
    if reclaimed < target {
        let freed = crate::block::cache::reclaim_clean_folios(target - reclaimed);
        if freed > 0 {
            debug!("page_cache_reclaim: evicted {freed} clean block-cache folios");
        }
        reclaimed += freed;
    }

    if reclaimed > 0 {
        debug!(
            "page_cache_reclaim: evicted {} clean pages across {} files",
            reclaimed, visited_files
        );
    }
    reclaimed
}

pub(super) fn register_cached_file(file: &Arc<CachedFileShared>) {
    prune_cached_files();
    GLOBAL_CACHED_FILES.write().push(file.clone());
}

/// Drops the reclaim registry's ownership after the backing inode is reaped.
///
/// The removed `Arc` is dropped only after releasing the registry spin lock:
/// destroying a cached file can take its sleepable page-cache lock.
#[cfg(feature = "ext4")]
pub(super) fn release_unlinked_cached_file(file: &Arc<CachedFileShared>) {
    let removed = {
        let mut registry = GLOBAL_CACHED_FILES.write();
        registry
            .iter()
            .position(|cached| Arc::ptr_eq(cached, file))
            .map(|index| registry.remove(index))
    };
    drop(removed);
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
    // Cached-file destruction can reach a sleepable filesystem lock. Move the
    // registry contents out under the spin lock, prune them after releasing
    // it, then merge survivors with registrations that arrived meanwhile.
    let mut files = {
        let mut registry = GLOBAL_CACHED_FILES.write();
        mem::take(&mut *registry)
    };
    files.retain(|cached| Arc::strong_count(cached) > 1 || cached.has_dirty_pages());
    GLOBAL_CACHED_FILES.write().append(&mut files);
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

#[cfg(test)]
fn reclaim_releases_registry_spin_lock_for_test() -> bool {
    const RECLAIM_PAGES: usize = 32;

    let file = Arc::new(CachedFileShared::new_unbounded(
        (RECLAIM_PAGES * crate::os::memory::PAGE_SIZE) as u64,
    ));
    for page_number in 0..RECLAIM_PAGES as u32 {
        let Ok(page) = PageCache::new() else {
            return false;
        };
        file.page_cache.lock().put(page_number, page);
    }

    let registry_was_unlocked = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&registry_was_unlocked);
    file.evict_listeners
        .lock()
        .push_back(alloc::boxed::Box::new(super::EvictListener {
            listener: Arc::new(move |_, _| {
                observed.store(GLOBAL_CACHED_FILES.try_write().is_some(), Ordering::Release);
                true
            }),
            writeback_protect: Arc::new(|_| true),
            link: intrusive_collections::LinkedListAtomicLink::new(),
        }));

    let registered = Arc::clone(&file);
    GLOBAL_CACHED_FILES.write().insert(0, registered);

    let reclaimed = page_cache_reclaim(1);
    let registered = {
        let mut registry = GLOBAL_CACHED_FILES.write();
        let index = registry
            .iter()
            .position(|cached| Arc::ptr_eq(cached, &file))
            .expect("reclaim test cached file disappeared from the registry");
        registry.remove(index)
    };
    drop(registered);

    reclaimed == RECLAIM_PAGES && registry_was_unlocked.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    #[test]
    fn reclaim_releases_registry_spin_lock_before_sleepable_file_locks() {
        with_test_page_provider(true, |_| {
            assert!(reclaim_releases_registry_spin_lock_for_test());
        });
    }

    #[cfg(feature = "ext4")]
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
